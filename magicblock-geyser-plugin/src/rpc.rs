use std::sync::{atomic::AtomicU64, Arc};

use expiring_hashmap::SharedMap;
use log::*;
use solana_sdk::{pubkey::Pubkey, signature::Signature};
use tokio::sync::{mpsc, Notify};

use crate::{
    config::ConfigGrpc,
    grpc::GrpcService,
    types::{
        geyser_message_channel, GeyserMessage, GeyserMessageSender,
        LogsSubscribeKey, SubscriptionsDb,
    },
    utils::{short_signature, CacheState},
};

pub struct GeyserRpcService {
    config: ConfigGrpc,
    subscribe_id: AtomicU64,
    pub subscriptions_db: SubscriptionsDb,
    transactions_cache: Option<SharedMap<Signature, GeyserMessage>>,
    accounts_cache: Option<SharedMap<Pubkey, GeyserMessage>>,
}

impl std::fmt::Debug for GeyserRpcService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let tx_cache = CacheState::from(self.transactions_cache.as_ref());
        let acc_cache = CacheState::from(self.accounts_cache.as_ref());
        f.debug_struct("GeyserRpcService")
            .field("config", &self.config)
            .field("subscribe_id", &self.subscribe_id)
            .field("transactions_cache", &tx_cache)
            .field("accounts_cache", &acc_cache)
            .finish()
    }
}

impl GeyserRpcService {
    #[allow(clippy::type_complexity)]
    pub fn create(
        config: ConfigGrpc,
        transactions_cache: Option<SharedMap<Signature, GeyserMessage>>,
        accounts_cache: Option<SharedMap<Pubkey, GeyserMessage>>,
    ) -> Result<
        (GeyserMessageSender, Arc<Notify>, Self),
        Box<dyn std::error::Error + Send + Sync>,
    > {
        let rpc_service = Self {
            subscribe_id: AtomicU64::new(0),
            config: config.clone(),
            transactions_cache,
            accounts_cache,
            subscriptions_db: SubscriptionsDb::default(),
        };

        // Run geyser message loop
        let (messages_tx, messages_rx) = geyser_message_channel();
        tokio::spawn(GrpcService::geyser_loop(
            messages_rx,
            rpc_service.subscriptions_db.clone(),
        ));

        // TODO: should Geyser handle shutdown or the piece that instantiates
        // the RPC service?
        let shutdown = Arc::new(Notify::new());
        Ok((messages_tx, shutdown, rpc_service))
    }

    // -----------------
    // Subscriptions
    // -----------------
    pub async fn accounts_subscribe(
        &self,
        subid: u64,
        pubkey: Pubkey,
    ) -> mpsc::Receiver<GeyserMessage> {
        let (updates_tx, updates_rx) =
            mpsc::channel(self.config.channel_capacity);
        let msg = self
            .accounts_cache
            .as_ref()
            .and_then(|cache| cache.get(&pubkey).clone());
        if let Some(msg) = msg {
            updates_tx
                .try_send(msg)
                .expect("channel should have at least 1 capacity");
        }
        self.subscriptions_db
            .subscribe_to_account(pubkey, updates_tx, subid)
            .await;

        updates_rx
    }

    pub async fn program_subscribe(
        &self,
        subid: u64,
        pubkey: Pubkey,
    ) -> mpsc::Receiver<GeyserMessage> {
        let (updates_tx, updates_rx) =
            mpsc::channel(self.config.channel_capacity);
        self.subscriptions_db
            .subscribe_to_program(pubkey, updates_tx, subid)
            .await;

        updates_rx
    }

    pub async fn transaction_subscribe(
        &self,
        subid: u64,
        signature: Signature,
    ) -> mpsc::Receiver<GeyserMessage> {
        let (updates_tx, updates_rx) =
            mpsc::channel(self.config.channel_capacity);
        let msg = self
            .transactions_cache
            .as_ref()
            .and_then(|cache| cache.get(&signature).clone());
        if let Some(msg) = msg {
            updates_tx
                .try_send(msg)
                .expect("channel should have at least 1 capacity");
        } else if log::log_enabled!(log::Level::Trace) {
            trace!("tx cache miss: '{}'", short_signature(&signature));
        }
        self.subscriptions_db
            .subscribe_to_signature(signature, updates_tx, subid)
            .await;

        updates_rx
    }

    pub async fn slot_subscribe(
        &self,
        subid: u64,
    ) -> mpsc::Receiver<GeyserMessage> {
        let (updates_tx, updates_rx) =
            mpsc::channel(self.config.channel_capacity);
        self.subscriptions_db
            .subscribe_to_slot(updates_tx, subid)
            .await;
        updates_rx
    }

    pub async fn logs_subscribe(
        &self,
        key: LogsSubscribeKey,
        subid: u64,
    ) -> mpsc::Receiver<GeyserMessage> {
        let (updates_tx, updates_rx) =
            mpsc::channel(self.config.channel_capacity);
        self.subscriptions_db
            .subscribe_to_logs(key, updates_tx, subid)
            .await;

        updates_rx
    }
}
