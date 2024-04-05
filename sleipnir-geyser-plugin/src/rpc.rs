#![allow(unused)]
use log::*;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};
use tonic::{Result as TonicResult, Status};

use geyser_grpc_proto::geyser::{
    CommitmentLevel, SubscribeRequest, SubscribeRequestFilterAccounts,
    SubscribeRequestFilterTransactions, SubscribeUpdate,
};
use tokio::sync::{broadcast, mpsc, Notify};

use crate::{
    config::{ConfigBlockFailAction, ConfigGrpc},
    filters::Filter,
    grpc::GrpcService,
    grpc_messages::{BlockMetaStorage, Message},
};

#[derive(Debug)]
pub struct GeyserRpcService {
    grpc_service: GrpcService,
    config: ConfigGrpc,
    broadcast_tx: broadcast::Sender<(CommitmentLevel, Arc<Vec<Message>>)>,
    subscribe_id: AtomicU64,
}

impl GeyserRpcService {
    pub async fn create(
        config: ConfigGrpc,
        block_fail_action: ConfigBlockFailAction,
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

    pub fn account_subscribe(
        &self,
        account_subscription: HashMap<String, SubscribeRequestFilterAccounts>,
    ) -> u64 {
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
        )
        .expect("empty filter");

        self.subscribe_impl(filter)
    }

    pub fn transaction_subscribe(
        &self,
        transaction_subscription: HashMap<
            String,
            SubscribeRequestFilterTransactions,
        >,
    ) -> u64 {
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
        )
        .expect("empty filter");

        self.subscribe_impl(filter)
    }

    fn subscribe_impl(&self, filter: Filter) -> u64 {
        // NOTE: this would run for each subscription that comes in
        let id = self.next_id();
        let (stream_tx, mut stream_rx) =
            mpsc::channel(self.config.channel_capacity);
        tokio::spawn(async move {
            loop {
                match stream_rx.recv().await {
                    Some(msg) => {
                        // TODO: here we would send to RPC sub
                        debug!("client: #{id} -> {:?}", msg);
                    }
                    None => error!("empty message"),
                }
            }
        });

        tokio::spawn(Self::client_loop(
            id,
            filter,
            stream_tx,
            self.broadcast_tx.subscribe(),
        ));

        id
    }

    async fn client_loop(
        id: u64,
        mut filter: Filter,
        stream_tx: mpsc::Sender<TonicResult<SubscribeUpdate>>,
        mut messages_rx: broadcast::Receiver<(
            CommitmentLevel,
            Arc<Vec<Message>>,
        )>,
    ) {
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
                    if commitment == filter.get_commitment_level() {
                        for message in messages.iter() {
                            for message in filter.get_update(message, Some(commitment)) {
                                match stream_tx.try_send(Ok(message)) {
                                    Ok(()) => {}
                                    Err(mpsc::error::TrySendError::Full(_)) => {
                                        error!("client #{id}: lagged to send update");
                                         tokio::spawn(async move {
                                             let _ = stream_tx.send(Err(Status::internal("lagged"))).await;
                                         });
                                        break 'outer;
                                    }
                                    Err(mpsc::error::TrySendError::Closed(_)) => {
                                        error!("client #{id}: stream closed");
                                        break 'outer;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
