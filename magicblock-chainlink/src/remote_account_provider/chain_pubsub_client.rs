use log::*;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use solana_pubkey::Pubkey;
use solana_sdk::commitment_config::CommitmentConfig;
use tokio::sync::{mpsc, oneshot};

use super::chain_pubsub_actor::{
    ChainPubsubActor, ChainPubsubActorMessage, SubscriptionUpdate,
};
use super::errors::RemoteAccountProviderResult;

// -----------------
// Trait
// -----------------
#[async_trait]
pub trait ChainPubsubClient: Send + Sync + Clone + 'static {
    async fn subscribe(
        &self,
        pubkey: Pubkey,
    ) -> RemoteAccountProviderResult<()>;
    async fn unsubscribe(
        &self,
        pubkey: Pubkey,
    ) -> RemoteAccountProviderResult<()>;
    async fn shutdown(&self);
    async fn recycle_connections(&self);

    fn take_updates(&self) -> mpsc::Receiver<SubscriptionUpdate>;
}

// -----------------
// Implementation
// -----------------
#[derive(Clone)]
pub struct ChainPubsubClientImpl {
    actor: Arc<ChainPubsubActor>,
    updates_rcvr: Arc<Mutex<Option<mpsc::Receiver<SubscriptionUpdate>>>>,
}

impl ChainPubsubClientImpl {
    pub async fn try_new_from_url(
        pubsub_url: &str,
        commitment: CommitmentConfig,
    ) -> RemoteAccountProviderResult<Self> {
        let (actor, updates) =
            ChainPubsubActor::new_from_url(pubsub_url, commitment).await?;
        Ok(Self {
            actor: Arc::new(actor),
            updates_rcvr: Arc::new(Mutex::new(Some(updates))),
        })
    }
}

#[async_trait]
impl ChainPubsubClient for ChainPubsubClientImpl {
    async fn shutdown(&self) {
        self.actor.shutdown().await;
    }

    async fn recycle_connections(&self) {
        // Fire a recycle request to the actor and await the acknowledgement.
        // If recycle fails there is nothing the caller could do, so we log an error instead
        let (tx, rx) = oneshot::channel();
        if let Err(err) = self
            .actor
            .send_msg(ChainPubsubActorMessage::RecycleConnections {
                response: tx,
            })
            .await
        {
            error!(
                "ChainPubsubClientImpl::recycle_connections: failed to send RecycleConnections: {err:?}"
            );
            return;
        }
        let res = match rx.await {
            Ok(r) => r,
            Err(err) => {
                error!(
                    "ChainPubsubClientImpl::recycle_connections: actor dropped recycle ack: {err:?}"
                );
                return;
            }
        };
        if let Err(err) = res {
            error!(
                "ChainPubsubClientImpl::recycle_connections: recycle failed: {err:?}"
            );
        }
    }

    fn take_updates(&self) -> mpsc::Receiver<SubscriptionUpdate> {
        // SAFETY: This can only be None if `take_updates` is called more than
        // once (double-take). That indicates a logic bug in the calling code.
        // Panicking here surfaces the bug early and prevents silently losing
        // the updates stream.
        self.updates_rcvr
            .lock()
            .unwrap()
            .take()
            .expect("ChainPubsubClientImpl::take_updates called more than once")
    }

    async fn subscribe(
        &self,
        pubkey: Pubkey,
    ) -> RemoteAccountProviderResult<()> {
        let (tx, rx) = oneshot::channel();
        self.actor
            .send_msg(ChainPubsubActorMessage::AccountSubscribe {
                pubkey,
                response: tx,
            })
            .await?;

        rx.await?
    }

    async fn unsubscribe(
        &self,
        pubkey: Pubkey,
    ) -> RemoteAccountProviderResult<()> {
        let (tx, rx) = oneshot::channel();
        self.actor
            .send_msg(ChainPubsubActorMessage::AccountUnsubscribe {
                pubkey,
                response: tx,
            })
            .await?;

        rx.await?
    }
}

// -----------------
// Mock
// -----------------
#[cfg(any(test, feature = "dev-context"))]
pub mod mock {
    use log::*;
    use solana_account::Account;
    use solana_account_decoder::{encode_ui_account, UiAccountEncoding};
    use solana_rpc_client_api::response::{
        Response as RpcResponse, RpcResponseContext,
    };
    use solana_sdk::clock::Slot;

    use super::*;
    use std::collections::HashSet;
    use std::sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    };

    #[derive(Clone)]
    pub struct ChainPubsubClientMock {
        updates_sndr: mpsc::Sender<SubscriptionUpdate>,
        updates_rcvr: Arc<Mutex<Option<mpsc::Receiver<SubscriptionUpdate>>>>,
        subscribed_pubkeys: Arc<Mutex<HashSet<Pubkey>>>,
        recycle_calls: Arc<AtomicU64>,
    }

    impl ChainPubsubClientMock {
        pub fn new(
            updates_sndr: mpsc::Sender<SubscriptionUpdate>,
            updates_rcvr: mpsc::Receiver<SubscriptionUpdate>,
        ) -> Self {
            Self {
                updates_sndr,
                updates_rcvr: Arc::new(Mutex::new(Some(updates_rcvr))),
                subscribed_pubkeys: Arc::new(Mutex::new(HashSet::new())),
                recycle_calls: Arc::new(AtomicU64::new(0)),
            }
        }

        pub fn recycle_calls(&self) -> u64 {
            self.recycle_calls.load(Ordering::SeqCst)
        }

        async fn send(&self, update: SubscriptionUpdate) {
            let subscribed_pubkeys =
                self.subscribed_pubkeys.lock().unwrap().clone();
            if subscribed_pubkeys.contains(&update.pubkey) {
                let _ =
                    self.updates_sndr.send(update).await.inspect_err(|err| {
                        error!("Failed to send subscription update: {err:?}")
                    });
            }
        }

        pub async fn send_account_update(
            &self,
            pubkey: Pubkey,
            slot: Slot,
            account: &Account,
        ) {
            let ui_acc = encode_ui_account(
                &pubkey,
                account,
                UiAccountEncoding::Base58,
                None,
                None,
            );
            let rpc_response = RpcResponse {
                context: RpcResponseContext {
                    slot,
                    api_version: None,
                },
                value: ui_acc,
            };
            self.send(SubscriptionUpdate {
                pubkey,
                rpc_response,
            })
            .await;
        }
    }

    #[async_trait]
    impl ChainPubsubClient for ChainPubsubClientMock {
        async fn recycle_connections(&self) {
            self.recycle_calls.fetch_add(1, Ordering::SeqCst);
        }

        fn take_updates(&self) -> mpsc::Receiver<SubscriptionUpdate> {
            // SAFETY: This can only be None if `take_updates` is called more
            // than once (double take). That would indicate a logic bug in the
            // calling code. Panicking here surfaces such a bug early and avoids
            // silently losing the updates stream.
            self.updates_rcvr.lock().unwrap().take().expect(
                "ChainPubsubClientMock::take_updates called more than once",
            )
        }
        async fn subscribe(
            &self,
            pubkey: Pubkey,
        ) -> RemoteAccountProviderResult<()> {
            let mut subscribed_pubkeys =
                self.subscribed_pubkeys.lock().unwrap();
            subscribed_pubkeys.insert(pubkey);
            Ok(())
        }

        async fn unsubscribe(
            &self,
            pubkey: Pubkey,
        ) -> RemoteAccountProviderResult<()> {
            let mut subscribed_pubkeys =
                self.subscribed_pubkeys.lock().unwrap();
            subscribed_pubkeys.remove(&pubkey);
            Ok(())
        }

        async fn shutdown(&self) {}
    }
}

// -----------------
// Tests
// -----------------
#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Mutex};

    use crate::{
        skip_if_no_test_validator,
        testing::utils::{airdrop, random_pubkey, PUBSUB_URL, RPC_URL},
    };

    use super::*;
    use solana_rpc_client::nonblocking::rpc_client::RpcClient;
    use solana_sdk::{clock::Clock, sysvar::clock};
    use tokio::task;

    async fn setup(
    ) -> (ChainPubsubClientImpl, mpsc::Receiver<SubscriptionUpdate>) {
        let _ = env_logger::builder().is_test(true).try_init();
        let client = ChainPubsubClientImpl::try_new_from_url(
            PUBSUB_URL,
            CommitmentConfig::confirmed(),
        )
        .await
        .unwrap();
        let updates = client.take_updates();
        (client, updates)
    }

    fn updates_to_lamports(updates: &[SubscriptionUpdate]) -> Vec<u64> {
        updates
            .iter()
            .map(|update| {
                let res = &update.rpc_response;
                res.value.lamports
            })
            .collect()
    }

    macro_rules! lamports {
        ($received_updates:ident, $pubkey:ident) => {
            $received_updates
                .lock()
                .unwrap()
                .get(&$pubkey)
                .map(|x| updates_to_lamports(x))
        };
    }

    fn updates_total_len(
        updates: &Mutex<HashMap<Pubkey, Vec<SubscriptionUpdate>>>,
    ) -> usize {
        updates
            .lock()
            .unwrap()
            .values()
            .map(|updates| updates.len())
            .sum()
    }

    async fn sleep_millis(millis: u64) {
        tokio::time::sleep(tokio::time::Duration::from_millis(millis)).await;
    }

    async fn wait_for_updates(
        updates: &Mutex<HashMap<Pubkey, Vec<SubscriptionUpdate>>>,
        starting_len: usize,
        amount: usize,
    ) {
        while updates_total_len(updates) < starting_len + amount {
            sleep_millis(100).await;
        }
    }

    #[tokio::test]
    async fn test_chain_pubsub_client_clock() {
        skip_if_no_test_validator!();
        const ITER: usize = 3;

        let (client, mut updates) = setup().await;

        client.subscribe(clock::ID).await.unwrap();
        let mut received_updates = vec![];
        while let Some(update) = updates.recv().await {
            received_updates.push(update);
            if received_updates.len() == ITER {
                break;
            }
        }
        client.shutdown().await;

        assert_eq!(received_updates.len(), ITER);

        let mut last_slot = None;
        for update in received_updates {
            let clock_data = update.rpc_response.value.data.decode().unwrap();
            let clock_value =
                bincode::deserialize::<Clock>(&clock_data).unwrap();
            // We show as part of this test that the context slot always matches
            // the clock slot which allows us to save on parsing in production since
            // we can just use the context slot instead of parsing the clock data.
            assert_eq!(update.rpc_response.context.slot, clock_value.slot);
            if let Some(last_slot) = last_slot {
                assert!(clock_value.slot > last_slot);
            } else {
                last_slot = Some(clock_value.slot);
            }
        }
    }

    #[tokio::test]
    async fn test_chain_pubsub_client_airdropping() {
        skip_if_no_test_validator!();

        let rpc_client = RpcClient::new_with_commitment(
            RPC_URL.to_string(),
            CommitmentConfig::confirmed(),
        );
        let (client, mut updates) = setup().await;

        let received_updates = {
            let map = HashMap::new();
            Arc::new(Mutex::new(map))
        };

        task::spawn({
            let received_updates = received_updates.clone();
            async move {
                while let Some(update) = updates.recv().await {
                    let mut map = received_updates.lock().unwrap();
                    map.entry(update.pubkey)
                        .or_insert_with(Vec::new)
                        .push(update);
                }
            }
        });

        let pubkey1 = random_pubkey();
        let pubkey2 = random_pubkey();

        {
            let len = updates_total_len(&received_updates);

            client.subscribe(pubkey1).await.unwrap();
            airdrop(&rpc_client, &pubkey1, 1_000_000).await;
            airdrop(&rpc_client, &pubkey2, 1_000_000).await;

            wait_for_updates(&received_updates, len, 1).await;

            let lamports1 =
                lamports!(received_updates, pubkey1).expect("pubkey1 missing");
            let lamports2 = lamports!(received_updates, pubkey2);

            assert_eq!(lamports1.len(), 1);
            assert_eq!(*lamports1.last().unwrap(), 1_000_000);
            assert_eq!(lamports2, None);
        }

        {
            let len = updates_total_len(&received_updates);

            client.subscribe(pubkey2).await.unwrap();
            airdrop(&rpc_client, &pubkey1, 2_000_000).await;
            airdrop(&rpc_client, &pubkey2, 2_000_000).await;

            wait_for_updates(&received_updates, len, 2).await;

            let lamports1 =
                lamports!(received_updates, pubkey1).expect("pubkey1 missing");
            let lamports2 =
                lamports!(received_updates, pubkey2).expect("pubkey2 missing");

            assert_eq!(lamports1.len(), 2);
            assert_eq!(*lamports1.last().unwrap(), 3_000_000);
            assert_eq!(lamports2.len(), 1);
            assert_eq!(*lamports2.last().unwrap(), 3_000_000);
        }

        {
            let len = updates_total_len(&received_updates);

            client.unsubscribe(pubkey1).await.unwrap();
            airdrop(&rpc_client, &pubkey1, 3_000_000).await;
            airdrop(&rpc_client, &pubkey2, 3_000_000).await;

            wait_for_updates(&received_updates, len, 1).await;

            let lamports1 =
                lamports!(received_updates, pubkey1).expect("pubkey1 missing");
            let lamports2 =
                lamports!(received_updates, pubkey2).expect("pubkey2 missing");

            assert_eq!(lamports1.len(), 2);
            assert_eq!(*lamports1.last().unwrap(), 3_000_000);
            assert_eq!(lamports2.len(), 2);
            assert_eq!(*lamports2.last().unwrap(), 6_000_000);
        }
    }
}
