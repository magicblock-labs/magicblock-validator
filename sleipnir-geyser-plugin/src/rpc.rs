#![allow(unused)]
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::Receiver,
        Arc,
    },
};

use geyser_grpc_proto::{
    geyser::{
        subscribe_update::UpdateOneof, CommitmentLevel, SubscribeRequest,
        SubscribeRequestFilterAccounts, SubscribeRequestFilterTransactions,
        SubscribeUpdate,
    },
    prelude::{SubscribeRequestFilterSlots, SubscribeUpdateSlot},
};
use log::*;
use solana_sdk::{pubkey::Pubkey, signature::Signature};
use stretto::Cache;
use tokio::sync::{broadcast, mpsc, Notify};
use tokio_util::sync::CancellationToken;
use tonic::{Result as TonicResult, Status};

use crate::{
    config::{ConfigBlockFailAction, ConfigGrpc},
    filters::Filter,
    grpc::GrpcService,
    grpc_messages::{BlockMetaStorage, Message},
    utils::{
        short_signature, short_signature_from_sub_update,
        short_signature_from_vec,
    },
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
    pub fn accounts_subscribe(
        &self,
        account_subscription: HashMap<String, SubscribeRequestFilterAccounts>,
        subid: u64,
        unsubscriber: CancellationToken,
        pubkey: &Pubkey,
    ) -> anyhow::Result<mpsc::Receiver<Result<SubscribeUpdate, Status>>> {
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

        let sub_update = self.subscribe_impl(filter, subid, unsubscriber, msgs);
        Ok(sub_update)
    }

    pub fn transaction_subscribe(
        &self,
        transaction_subscription: HashMap<
            String,
            SubscribeRequestFilterTransactions,
        >,
        subid: u64,
        unsubscriber: CancellationToken,
        signature: &Signature,
    ) -> anyhow::Result<mpsc::Receiver<Result<SubscribeUpdate, Status>>> {
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

        if log::log_enabled!(log::Level::Trace)
            && msgs.as_ref().map(|val| val.is_empty()).unwrap_or_default()
        {
            trace!("tx cache miss: '{}'", short_signature(signature));
        }

        let sub_update = self.subscribe_impl(filter, subid, unsubscriber, msgs);

        Ok(sub_update)
    }

    pub fn slot_subscribe(
        &self,
        slot_subscription: HashMap<String, SubscribeRequestFilterSlots>,
        subid: u64,
        unsubscriber: CancellationToken,
    ) -> anyhow::Result<mpsc::Receiver<Result<SubscribeUpdate, Status>>> {
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
        let sub_update = self.subscribe_impl(filter, subid, unsubscriber, None);

        Ok(sub_update)
    }

    fn subscribe_impl(
        &self,
        filter: Filter,
        subid: u64,
        unsubscriber: CancellationToken,
        initial_messages: Option<Arc<Vec<Message>>>,
    ) -> mpsc::Receiver<Result<SubscribeUpdate, Status>> {
        let (stream_tx, mut stream_rx) =
            mpsc::channel(self.config.channel_capacity);

        tokio::spawn(Self::client_loop(
            subid,
            filter,
            stream_tx,
            unsubscriber,
            self.broadcast_tx.subscribe(),
            initial_messages,
        ));

        stream_rx
    }

    /// Sends messages that could be interesting to the subscriber and then listend for more
    /// messages.
    /// By using the same transport as future messages we ensure to use the same logic WRT
    /// filters.
    async fn client_loop(
        subid: u64,
        mut filter: Filter,
        stream_tx: mpsc::Sender<TonicResult<SubscribeUpdate>>,
        unsubscriber: CancellationToken,
        mut messages_rx: broadcast::Receiver<(
            CommitmentLevel,
            Arc<Vec<Message>>,
        )>,
        mut initial_messages: Option<Arc<Vec<Message>>>,
    ) {
        // 1. Send initial messages that were cached from previous updates
        if let Some(messages) = initial_messages.take() {
            let exit = handle_messages(
                subid,
                unsubscriber.clone(),
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
            let unsubscriber = unsubscriber.clone();
            tokio::select! {
                message = messages_rx.recv() => {
                    let (commitment, messages) = match message {
                        Ok((commitment, messages)) => (commitment, messages),
                        Err(broadcast::error::RecvError::Closed) => {
                            break 'outer;
                        },
                        Err(broadcast::error::RecvError::Lagged(_)) => {
                            info!("client #{subid}: lagged to receive geyser messages");
                            // tokio::spawn(async move {
                            //     let _ = stream_tx.send(Err(Status::internal("lagged"))).await;
                            // });
                            break 'outer;
                        }
                    };
                    let exit_loop = handle_messages(
                        subid,
                        unsubscriber,
                        &filter,
                        commitment,
                        messages,
                        &stream_tx
                    );
                    if exit_loop {
                        break 'outer;
                    }
                }
                _ = unsubscriber.cancelled() => {
                    break 'outer;
                }
            }
        }
    }
}

fn handle_messages(
    subid: u64,
    unsubscriber: CancellationToken,
    filter: &Filter,
    commitment: CommitmentLevel,
    messages: Arc<Vec<Message>>,
    stream_tx: &mpsc::Sender<TonicResult<SubscribeUpdate>>,
) -> bool {
    if commitment == filter.get_commitment_level() {
        for message in messages.iter() {
            for message in filter.get_update(message, Some(commitment)) {
                if unsubscriber.is_cancelled() {
                    return true;
                }
                if log::log_enabled!(log::Level::Trace) {
                    if let Some(UpdateOneof::Transaction(tx)) =
                        message.update_oneof.as_ref()
                    {
                        trace!(
                            "sending tx: '{}'",
                            short_signature_from_sub_update(tx)
                        );
                    };
                }
                match stream_tx.try_send(Ok(message)) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        error!("client #{subid}: lagged to send update");
                        let stream_tx = stream_tx.clone();
                        tokio::spawn(async move {
                            let _ = stream_tx
                                .send(Err(Status::internal("lagged")))
                                .await;
                        });
                        return true;
                    }
                    Err(mpsc::error::TrySendError::Closed(status)) => {
                        // This happens more often than we'd like.
                        // This could either be due to the client not properly unsubscribing,
                        // or due to the fact that the cancellation future doesn't get polled
                        // while we're processing code synchronously here.
                        // However it isn't critical as we know to stop the client subscription
                        // loop in either case.
                        trace!(
                            "client #{subid}: stream closed {}",
                            if unsubscriber.is_cancelled() {
                                "cancelled"
                            } else {
                                "uncancelled"
                            }
                        );
                        return true;
                    }
                }
            }
        }
    }
    false
}
