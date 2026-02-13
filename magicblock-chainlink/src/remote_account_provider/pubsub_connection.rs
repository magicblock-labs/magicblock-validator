use std::{mem, sync::Arc, time::Duration};

use arc_swap::ArcSwap;
use async_trait::async_trait;
use futures_util::stream::BoxStream;
use solana_account_decoder::UiAccount;
use solana_pubkey::Pubkey;
use solana_pubsub_client::nonblocking::pubsub_client::{
    PubsubClient, PubsubClientResult,
};
use solana_rpc_client_api::{
    config::{RpcAccountInfoConfig, RpcProgramAccountsConfig},
    response::{Response, RpcKeyedAccount},
};
use tokio::{sync::Mutex as AsyncMutex, time};
use tracing::warn;

use super::errors::RemoteAccountProviderResult;

pub type UnsubscribeFn =
    Box<dyn FnOnce() -> futures_util::future::BoxFuture<'static, ()> + Send>;
pub type SubscribeResult = PubsubClientResult<(
    BoxStream<'static, Response<UiAccount>>,
    UnsubscribeFn,
)>;
pub type ProgramSubscribeResult = PubsubClientResult<(
    BoxStream<'static, Response<RpcKeyedAccount>>,
    UnsubscribeFn,
)>;

const MAX_RECONNECT_ATTEMPTS: usize = 5;
const RECONNECT_ATTEMPT_DELAY: Duration = Duration::from_millis(500);
const MAX_SUBSCRIBE_RETRIES: usize = 5;

#[async_trait]
pub trait PubsubConnection: Send + Sync + 'static {
    async fn new(url: String) -> RemoteAccountProviderResult<Self>
    where
        Self: Sized;
    fn url(&self) -> &str;
    async fn account_subscribe(
        &self,
        pubkey: &Pubkey,
        config: RpcAccountInfoConfig,
    ) -> SubscribeResult;
    async fn program_subscribe(
        &self,
        program_id: &Pubkey,
        config: RpcProgramAccountsConfig,
    ) -> ProgramSubscribeResult;
    async fn reconnect(&self) -> PubsubClientResult<()>;
}

pub struct PubsubConnectionImpl {
    client: ArcSwap<PubsubClient>,
    url: String,
    reconnect_guard: AsyncMutex<()>,
}

/// Retry loop for subscribe calls on `PubsubConnectionImpl`.
///
/// Loads the current client, calls `$client.$method($($args),*)`,
/// transmutes the resulting stream to `'static`, and retries with
/// linear backoff on failure.
/// NOTE: this macro approach works best here due to lifetime issues and overly
/// complicated generic types when using a function based approach.
macro_rules! subscribe_with_retry {
    ($self:expr, $kind:expr, $method:ident, $stream_item:ty,
     $($args:expr),* $(,)?) => {{
        let mut retries = MAX_SUBSCRIBE_RETRIES;
        let initial_tries = retries;

        loop {
            // SAFETY:
            // the returned stream depends on the used client, which
            // is only ever dropped if the connection has been
            // terminated, at which point the stream is useless and
            // will be discarded as well, thus it's safe lifetime
            // extension to 'static
            let result = {
                let client = $self.client.load();
                client.$method($($args),*).await.map(
                    |(stream, unsub)| {
                        let stream: BoxStream<
                            'static,
                            $stream_item,
                        > = unsafe { mem::transmute(stream) };
                        (stream, unsub)
                    },
                )
            };

            match result {
                Ok(ok) => break Ok(ok),
                Err(err) => {
                    if retries > 0 {
                        retries -= 1;
                        let backoff_ms =
                            50u64 * (initial_tries - retries) as u64;
                        time::sleep(Duration::from_millis(backoff_ms))
                            .await;
                        continue;
                    }
                    if initial_tries > 0 {
                        warn!(
                            error = ?err,
                            retries_count = initial_tries,
                            "Failed to subscribe to {} after \
                             retrying multiple times",
                            $kind,
                        );
                    }
                    break Err(err);
                }
            }
        }
    }};
}

#[async_trait]
impl PubsubConnection for PubsubConnectionImpl {
    async fn new(url: String) -> RemoteAccountProviderResult<Self> {
        let client = Arc::new(PubsubClient::new(&url).await?).into();
        let reconnect_guard = AsyncMutex::new(());
        Ok(Self {
            client,
            url,
            reconnect_guard,
        })
    }
    fn url(&self) -> &str {
        &self.url
    }

    async fn account_subscribe(
        &self,
        pubkey: &Pubkey,
        config: RpcAccountInfoConfig,
    ) -> SubscribeResult {
        let config = Some(config);
        subscribe_with_retry!(
            self,
            "account",
            account_subscribe,
            Response<UiAccount>,
            pubkey,
            config.clone(),
        )
    }

    async fn program_subscribe(
        &self,
        program_id: &Pubkey,
        config: RpcProgramAccountsConfig,
    ) -> ProgramSubscribeResult {
        let config = Some(config);
        subscribe_with_retry!(
            self,
            "program",
            program_subscribe,
            Response<RpcKeyedAccount>,
            program_id,
            config.clone(),
        )
    }

    async fn reconnect(&self) -> PubsubClientResult<()> {
        // Prevents multiple reconnect attempts running concurrently
        let _guard = match self.reconnect_guard.try_lock() {
            Ok(g) => g,
            // Reconnect is already in progress
            Err(_) => {
                // Wait a bit and return to retry subscription
                time::sleep(RECONNECT_ATTEMPT_DELAY).await;
                return Ok(());
            }
        };
        let mut attempt = 1;
        let client = loop {
            match PubsubClient::new(&self.url).await {
                Ok(c) => break Arc::new(c),
                Err(error) => {
                    warn!(
                        "failed to reconnect to ws endpoint at {} {error}",
                        self.url
                    );
                    if attempt == MAX_RECONNECT_ATTEMPTS {
                        return Err(error);
                    }
                    attempt += 1;
                    time::sleep(RECONNECT_ATTEMPT_DELAY).await;
                }
            }
        };
        self.client.store(client);
        Ok(())
    }
}

#[cfg(test)]
pub mod mock {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[derive(Clone)]
    pub struct MockPubsubConnection {
        account_subscriptions: Arc<Mutex<Vec<Pubkey>>>,
        program_subscriptions: Arc<Mutex<Vec<Pubkey>>>,
    }

    impl MockPubsubConnection {
        pub fn new() -> Self {
            Self {
                account_subscriptions: Arc::new(Mutex::new(Vec::new())),
                program_subscriptions: Arc::new(Mutex::new(Vec::new())),
            }
        }

        pub fn account_subs(&self) -> Vec<Pubkey> {
            self.account_subscriptions.lock().unwrap().clone()
        }

        pub fn program_subs(&self) -> Vec<Pubkey> {
            self.program_subscriptions.lock().unwrap().clone()
        }

        pub fn clear(&self) {
            self.account_subscriptions.lock().unwrap().clear();
            self.program_subscriptions.lock().unwrap().clear();
        }
    }

    impl Default for MockPubsubConnection {
        fn default() -> Self {
            Self::new()
        }
    }

    #[async_trait]
    impl PubsubConnection for MockPubsubConnection {
        async fn new(_url: String) -> RemoteAccountProviderResult<Self>
        where
            Self: Sized,
        {
            Ok(Self::new())
        }
        fn url(&self) -> &str {
            "mock://"
        }

        async fn account_subscribe(
            &self,
            pubkey: &Pubkey,
            _config: RpcAccountInfoConfig,
        ) -> SubscribeResult {
            self.account_subscriptions.lock().unwrap().push(*pubkey);

            // Return empty stream with no-op unsubscribe
            let stream = Box::pin(futures_util::stream::empty());
            let unsubscribe: UnsubscribeFn = Box::new(|| Box::pin(async {}));
            Ok((stream, unsubscribe))
        }

        async fn program_subscribe(
            &self,
            program_id: &Pubkey,
            _config: RpcProgramAccountsConfig,
        ) -> ProgramSubscribeResult {
            self.program_subscriptions.lock().unwrap().push(*program_id);

            // Return empty stream with no-op unsubscribe
            let stream = Box::pin(futures_util::stream::empty());
            let unsubscribe: UnsubscribeFn = Box::new(|| Box::pin(async {}));
            Ok((stream, unsubscribe))
        }

        async fn reconnect(&self) -> PubsubClientResult<()> {
            Ok(())
        }
    }
}
