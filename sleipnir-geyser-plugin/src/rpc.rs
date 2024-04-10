#![allow(unused)]
use log::*;
use solana_sdk::{pubkey::Pubkey, signature::Signature};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::Receiver,
        Arc,
    },
};
use stretto::Cache;
use tonic::{Result as TonicResult, Status};

use geyser_grpc_proto::geyser::{
    CommitmentLevel, SubscribeRequest, SubscribeRequestFilterAccounts,
    SubscribeRequestFilterTransactions, SubscribeUpdate,
};
use tokio::sync::{broadcast, mpsc, Notify};
use geyser_grpc_proto::prelude::{SubscribeRequestFilterSlots, SubscribeUpdateSlot};

use crate::{
    config::{ConfigBlockFailAction, ConfigGrpc},
    filters::Filter,
    grpc::GrpcService,
    grpc_messages::{BlockMetaStorage, Message},
};

pub struct GeyserRpcService {
    grpc_service: GrpcService,
    config: ConfigGrpc,
    broadcast_tx: broadcast::Sender<(CommitmentLevel, Arc<Vec<Message>>)>,
    subscribe_id: AtomicU64,

    transactions_cache: Cache<Signature, Message>,
    accounts_cache: Cache<Pubkey, Message>,
}

impl std::fmt::Debug for GeyserRpcService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeyserRpcService")
            .field("grpc_service", &self.grpc_service)
            .field("config", &self.config)
            .field("broadcast_tx", &self.broadcast_tx)
            .field("subscribe_id", &self.subscribe_id)
            .field("transactions_cache_size", &self.transactions_cache.len())
            .finish()
    }
}

impl GeyserRpcService {
    #[allow(clippy::type_complexity)]
    pub fn create(
        config: ConfigGrpc,
        block_fail_action: ConfigBlockFailAction,
        transactions_cache: Cache<Signature, Message>,
        accounts_cache: Cache<Pubkey, Message>,
    ) -> Result<
        (mpsc::UnboundedSender<Message>, Arc<Notify>, Self),
        Box<dyn std::error::Error + Send + Sync>,
    > {
        // Blocks meta storage
        let (blocks_meta, blocks_meta_tx) = if config.unary_disabled {
            (None, None)
        } else {
            let (blocks_meta, blocks_meta_tx) =
                BlockMetaStorage::new(config.unary_concurrency_limit);
            (Some(blocks_meta), Some(blocks_meta_tx))
        };

        // Messages to clients combined by commitment
        let (broadcast_tx, _) = broadcast::channel(config.channel_capacity);

        let rpc_service = Self {
            subscribe_id: AtomicU64::new(0),
            broadcast_tx: broadcast_tx.clone(),
            config: config.clone(),
            grpc_service: GrpcService::new(
                config,
                blocks_meta,
                broadcast_tx.clone(),
            ),
            transactions_cache,
            accounts_cache,
        };

        // Run geyser message loop
        let (messages_tx, messages_rx) = mpsc::unbounded_channel();
        tokio::spawn(GrpcService::geyser_loop(
            messages_rx,
            blocks_meta_tx,
            broadcast_tx.clone(),
            block_fail_action,
        ));

        // TODO: should Geyser handle shutdown or the piece that instantiates
        // the RPC service?
        let shutdown = Arc::new(Notify::new());
        Ok((messages_tx, shutdown, rpc_service))
    }

    // -----------------
    // Subscriptions
    // -----------------
    fn next_id(&self) -> u64 {
        self.subscribe_id.fetch_add(1, Ordering::SeqCst)
    }

    pub fn accounts_subscribe(
        &self,
        account_subscription: HashMap<String, SubscribeRequestFilterAccounts>,
        pubkey: &Pubkey,
    ) -> anyhow::Result<(u64, mpsc::Receiver<Result<SubscribeUpdate, Status>>)>
    {
        let filter = Filter::new(
            &SubscribeRequest {
                accounts: account_subscription,
                slots: HashMap::new(),
                transactions: HashMap::new(),
                blocks: HashMap::new(),
                blocks_meta: HashMap::new(),
                entry: HashMap::new(),
                commitment: None,
                accounts_data_slice: Vec::new(),
                ping: None,
            },
            &self.config.filters,
            self.config.normalize_commitment_level,
        )?;

        let msgs = self
            .accounts_cache
            .get(pubkey)
            .as_ref()
            .map(|val| Arc::new(vec![val.value().clone()]));
        let sub_update = self.subscribe_impl(filter, msgs);

        Ok(sub_update)
    }

    pub fn transaction_subscribe(
        &self,
        transaction_subscription: HashMap<
            String,
            SubscribeRequestFilterTransactions,
        >,
        signature: &Signature,
    ) -> anyhow::Result<(u64, mpsc::Receiver<Result<SubscribeUpdate, Status>>)>
    {
        debug!("tx sub, cache size {}", self.transactions_cache.len());
        let filter = Filter::new(
            &SubscribeRequest {
                accounts: HashMap::new(),
                slots: HashMap::new(),
                transactions: transaction_subscription,
                blocks: HashMap::new(),
                blocks_meta: HashMap::new(),
                entry: HashMap::new(),
                commitment: None,
                accounts_data_slice: Vec::new(),
                ping: None,
            },
            &self.config.filters,
            self.config.normalize_commitment_level,
        )?;
        let msgs = self
            .transactions_cache
            .get(signature)
            .as_ref()
            .map(|val| Arc::new(vec![val.value().clone()]));
        let sub_update = self.subscribe_impl(filter, msgs);

        Ok(sub_update)
    }

    pub fn slot_subscribe(
        &self,
        slot_subscription: HashMap<String, SubscribeRequestFilterSlots>,
    ) -> anyhow::Result<(u64, mpsc::Receiver<Result<SubscribeUpdate, Status>>)>
    {
        // We don't filter by slot for the RPC interface
        let filter = Filter::new(
            &SubscribeRequest {
                accounts: HashMap::new(),
                slots: slot_subscription,
                transactions: HashMap::new(),
                blocks: HashMap::new(),
                blocks_meta: HashMap::new(),
                entry: HashMap::new(),
                commitment: None,
                accounts_data_slice: Vec::new(),
                ping: None,
            },
            &self.config.filters,
            self.config.normalize_commitment_level,
        )?;
        let sub_update = self.subscribe_impl(filter, None);

        Ok(sub_update)
    }

    fn subscribe_impl(
        &self,
        filter: Filter,
        initial_messages: Option<Arc<Vec<Message>>>,
    ) -> (u64, mpsc::Receiver<Result<SubscribeUpdate, Status>>) {
        let id = self.next_id();
        let (stream_tx, mut stream_rx) =
            mpsc::channel(self.config.channel_capacity);

        tokio::spawn(Self::client_loop(
            id,
            filter,
            stream_tx,
            self.broadcast_tx.subscribe(),
            initial_messages,
        ));

        (id, stream_rx)
    }

    /// Sends messages that could be interesting to the subscriber and then listend for more
    /// messages.
    /// By using the same transport as future messages we ensure to use the same logic WRT
    /// filters.
    async fn client_loop(
        id: u64,
        mut filter: Filter,
        stream_tx: mpsc::Sender<TonicResult<SubscribeUpdate>>,
        mut messages_rx: broadcast::Receiver<(
            CommitmentLevel,
            Arc<Vec<Message>>,
        )>,
        mut initial_messages: Option<Arc<Vec<Message>>>,
    ) {
        // 1. Send initial messages that were cached from previous updates
        if let Some(messages) = initial_messages.take() {
            let exit = handle_messages(
                id,
                &filter,
                filter.get_commitment_level(),
                messages,
                &stream_tx,
            );
            if exit {
                return;
            }
        }
        // 2. Listen for future updates
        'outer: loop {
            tokio::select! {
                message = messages_rx.recv() => {
                    let (commitment, messages) = match message {
                        Ok((commitment, messages)) => (commitment, messages),
                        Err(broadcast::error::RecvError::Closed) => {
                            break 'outer;
                        },
                        Err(broadcast::error::RecvError::Lagged(_)) => {
                            info!("client #{id}: lagged to receive geyser messages");
                            // tokio::spawn(async move {
                            //     let _ = stream_tx.send(Err(Status::internal("lagged"))).await;
                            // });
                            break 'outer;
                        }
                    };
                    let exit_loop = handle_messages(id, &filter, commitment, messages, &stream_tx);
                    if exit_loop {
                        break 'outer;
                    }
                }
            }
        }
    }
}

fn handle_messages(
    id: u64,
    filter: &Filter,
    commitment: CommitmentLevel,
    messages: Arc<Vec<Message>>,
    stream_tx: &mpsc::Sender<TonicResult<SubscribeUpdate>>,
) -> bool {
    if commitment == filter.get_commitment_level() {
        for message in messages.iter() {
            for message in filter.get_update(message, Some(commitment)) {
                match stream_tx.try_send(Ok(message)) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        error!("client #{id}: lagged to send update");
                        let stream_tx = stream_tx.clone();
                        tokio::spawn(async move {
                            let _ = stream_tx
                                .send(Err(Status::internal("lagged")))
                                .await;
                        });
                        return true;
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        error!("client #{id}: stream closed");
                        return true;
                    }
                }
            }
        }
    }
    false
}
