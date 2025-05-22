#![allow(unused)]
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::Receiver,
        Arc,
    },
};

use expiring_hashmap::{ExpiringHashMap as Cache, SharedMap};
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
use tokio::sync::{broadcast, mpsc, Notify};
use tokio_util::sync::CancellationToken;
use tonic::{Result as TonicResult, Status};

use crate::{
    config::{ConfigBlockFailAction, ConfigGrpc},
    filters::Filter,
    grpc::GrpcService,
    grpc_messages::{BlockMetaStorage, Message},
    types::{
        geyser_message_channel, GeyserMessage, GeyserMessageSender,
        GeyserMessages, SubscriptionsDb,
    },
    utils::{
        short_signature, short_signature_from_sub_update,
        short_signature_from_vec, CacheState,
    },
};

pub struct GeyserRpcService {
    config: ConfigGrpc,
    subscribe_id: AtomicU64,
    subscriptions_db: SubscriptionsDb,
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
        block_fail_action: ConfigBlockFailAction,
        transactions_cache: Option<SharedMap<Signature, GeyserMessage>>,
        accounts_cache: Option<SharedMap<Pubkey, GeyserMessage>>,
    ) -> Result<
        (GeyserMessageSender, Arc<Notify>, Self),
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
    pub fn accounts_subscribe(
        &self,
        account_subscription: HashMap<String, SubscribeRequestFilterAccounts>,
        subid: u64,
        unsubscriber: CancellationToken,
        pubkey: Option<&Pubkey>,
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

        let msgs = self.accounts_cache.as_ref().and_then(|cache| {
            pubkey.and_then(|pubkey| {
                cache
                    .get(pubkey)
                    .as_ref()
                    .map(|val| Arc::new(vec![val.clone()]))
            })
        });

        let sub_update = self.subscribe_impl(subid, unsubscriber, msgs);
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
        signature: Option<&Signature>,
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
        let msgs = self.transactions_cache.as_ref().and_then(|cache| {
            signature.and_then(|signature| {
                let msgs = cache
                    .get(signature)
                    .as_ref()
                    .map(|val| Arc::new(vec![val.clone()]));

                if log::log_enabled!(log::Level::Trace)
                    && msgs
                        .as_ref()
                        .map(|val| val.is_empty())
                        .unwrap_or_default()
                {
                    trace!("tx cache miss: '{}'", short_signature(signature));
                }
                msgs
            })
        });

        let sub_update = self.subscribe_impl(subid, unsubscriber, msgs);

        Ok(sub_update)
    }

    pub fn slot_subscribe(
        &self,
        slot_subscription: HashMap<String, SubscribeRequestFilterSlots>,
        subid: u64,
        unsubscriber: CancellationToken,
    ) -> anyhow::Result<mpsc::Receiver<Result<SubscribeUpdate, Status>>> {
        // We don't filter by slot for the RPC interface
        let sub_update = self.subscribe_impl(subid, unsubscriber, None);

        Ok(sub_update)
    }

    fn subscribe_impl(
        &self,
        subid: u64,
        unsubscriber: CancellationToken,
        initial_messages: Option<GeyserMessages>,
    ) -> mpsc::Receiver<Result<SubscribeUpdate, Status>> {
        let (stream_tx, mut stream_rx) =
            mpsc::channel(self.config.channel_capacity);
        let (updates_rx, updates_tx) =
            mpsc::channel(self.config.channel_capacity);

        tokio::spawn(Self::client_loop(
            subid,
            stream_tx,
            unsubscriber,
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
        stream_tx: mpsc::Sender<TonicResult<SubscribeUpdate>>,
        unsubscriber: CancellationToken,
        mut messages_rx: broadcast::Receiver<(CommitmentLevel, GeyserMessages)>,
        mut initial_messages: Option<GeyserMessages>,
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
    messages: GeyserMessages,
    stream_tx: &mpsc::Sender<GeyserMessage>,
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
