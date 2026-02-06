use std::{mem, sync::Arc};

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
const RECONNECT_ATTEMPT_DELAY: std::time::Duration =
    std::time::Duration::from_millis(500);

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
        let client = self.client.load();
        let config = Some(config.clone());
        let (stream, unsub) = client.account_subscribe(pubkey, config).await?;
        // SAFETY:
        // the returned stream depends on the used client, which is only ever
        // dropped if the connection has been terminated, at which point the
        // stream is useless and will be discarded as well, thus it's safe
        // lifetime extension to 'static
        let stream = unsafe {
            mem::transmute::<
                BoxStream<'_, Response<UiAccount>>,
                BoxStream<'static, Response<UiAccount>>,
            >(stream)
        };
        Ok((stream, unsub))
    }

    async fn program_subscribe(
        &self,
        program_id: &Pubkey,
        config: RpcProgramAccountsConfig,
    ) -> ProgramSubscribeResult {
        let client = self.client.load();
        let config = Some(config.clone());
        let (stream, unsub) =
            client.program_subscribe(program_id, config).await?;

        // SAFETY:
        // the returned stream depends on the used client, which is only ever
        // dropped if the connection has been terminated, at which point the
        // stream is useless and will be discarded as well, thus it's safe
        // lifetime extension to 'static
        let stream = unsafe {
            mem::transmute::<
                BoxStream<'_, Response<RpcKeyedAccount>>,
                BoxStream<'static, Response<RpcKeyedAccount>>,
            >(stream)
        };
        Ok((stream, unsub))
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
    use super::*;
    use std::sync::Arc;
    use tokio::sync::Mutex;

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

        pub async fn account_subs(&self) -> Vec<Pubkey> {
            self.account_subscriptions.lock().await.clone()
        }

        pub async fn program_subs(&self) -> Vec<Pubkey> {
            self.program_subscriptions.lock().await.clone()
        }

        pub async fn clear(&self) {
            self.account_subscriptions.lock().await.clear();
            self.program_subscriptions.lock().await.clear();
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
            self.account_subscriptions.lock().await.push(*pubkey);

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
            self.program_subscriptions.lock().await.push(*program_id);

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
