// Adapted yellowstone-grpc/yellowstone-grpc-geyser/src/grpc.rs
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use geyser_grpc_proto::prelude::{
    geyser_server::{Geyser, GeyserServer},
    subscribe_update::UpdateOneof,
    CommitmentLevel, GetBlockHeightRequest, GetBlockHeightResponse,
    GetLatestBlockhashRequest, GetLatestBlockhashResponse, GetSlotRequest,
    GetSlotResponse, GetVersionRequest, GetVersionResponse,
    IsBlockhashValidRequest, IsBlockhashValidResponse, PingRequest,
    PongResponse, SubscribeRequest, SubscribeUpdate, SubscribeUpdatePing,
};
use log::{error, info};
use tokio::{
    sync::{broadcast, mpsc, Notify},
    time::{sleep, Duration},
};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{
    codec::CompressionEncoding,
    transport::server::{Server, TcpIncoming},
    Request, Response, Result as TonicResult, Status, Streaming,
};
use tonic_health::server::health_reporter;

use crate::{
    config::ConfigGrpc,
    filters::Filter,
    grpc_messages::*,
    types::{
        geyser_message_channel, GeyserMessageReceiver, GeyserMessageSender,
        GeyserMessages, SubscriptionsDb,
    },
    version::GrpcVersionInfo,
};

#[derive(Debug)]
pub struct GrpcService {
    config: ConfigGrpc,
    blocks_meta: Option<BlockMetaStorage>,
    subscribe_id: AtomicUsize,
    broadcast_tx: broadcast::Sender<(CommitmentLevel, GeyserMessages)>,
}

impl GrpcService {
    pub(crate) fn new(
        config: ConfigGrpc,
        blocks_meta: Option<BlockMetaStorage>,
        broadcast_tx: broadcast::Sender<(CommitmentLevel, GeyserMessages)>,
    ) -> Self {
        Self {
            config,
            blocks_meta,
            subscribe_id: AtomicUsize::new(0),
            broadcast_tx,
        }
    }

    #[allow(clippy::type_complexity)]
    pub fn create(
        config: ConfigGrpc,
    ) -> Result<
        (GeyserMessageSender, Arc<Notify>),
        Box<dyn std::error::Error + Send + Sync>,
    > {
        // Bind service address
        let incoming = TcpIncoming::new(
            config.address,
            true,                          // tcp_nodelay
            Some(Duration::from_secs(20)), // tcp_keepalive
        )?;

        // Blocks meta storage
        let (blocks_meta, _) = if config.unary_disabled {
            (None, None)
        } else {
            let (blocks_meta, blocks_meta_tx) =
                BlockMetaStorage::new(config.unary_concurrency_limit);
            (Some(blocks_meta), Some(blocks_meta_tx))
        };

        // Messages to clients combined by commitment
        let (broadcast_tx, _): (
            broadcast::Sender<(CommitmentLevel, GeyserMessages)>,
            broadcast::Receiver<(CommitmentLevel, GeyserMessages)>,
        ) = broadcast::channel(config.channel_capacity);

        // gRPC server builder
        let server_builder = Server::builder();

        // Create Server
        let max_decoding_message_size = config.max_decoding_message_size;
        let service = GeyserServer::new(Self::new(
            config,
            blocks_meta,
            broadcast_tx.clone(),
        ))
        .accept_compressed(CompressionEncoding::Gzip)
        .send_compressed(CompressionEncoding::Gzip)
        .max_decoding_message_size(max_decoding_message_size);

        // Run geyser message loop
        let (messages_tx, messages_rx) = geyser_message_channel();
        tokio::spawn(Self::geyser_loop(
            messages_rx,
            // Note: gRPC plugin is currently disabled, so it doesn't
            // matter that we pass exclusive SubscriptionsDb
            SubscriptionsDb::default(),
        ));

        // Run Server
        let shutdown = Arc::new(Notify::new());
        let shutdown_grpc = Arc::clone(&shutdown);
        tokio::spawn(async move {
            // gRPC Health check service
            let (mut health_reporter, health_service) = health_reporter();
            health_reporter.set_serving::<GeyserServer<Self>>().await;

            server_builder
                .http2_keepalive_interval(Some(Duration::from_secs(5)))
                .add_service(health_service)
                .add_service(service)
                .serve_with_incoming_shutdown(
                    incoming,
                    shutdown_grpc.notified(),
                )
                .await
        });

        Ok((messages_tx, shutdown))
    }

    pub(crate) async fn geyser_loop(
        messages_rx: GeyserMessageReceiver,
        subscriptions_db: SubscriptionsDb,
    ) {
        while let Ok(message) = messages_rx.recv() {
            match *message {
                Message::Slot(_) => subscriptions_db.send_slot(message),
                Message::Account(ref account) => {
                    let pubkey = account.account.pubkey;
                    subscriptions_db.send_account_update(&pubkey, message);
                }
                Message::Transaction(ref txn) => {
                    let signature = txn.transaction.signature;
                    subscriptions_db.send_signature_update(&signature, message);
                }
                Message::Block(_) => {
                    subscriptions_db.send_block(message);
                }
                _ => (),
            }
        }
    }

    async fn client_loop(
        id: usize,
        mut filter: Filter,
        stream_tx: mpsc::Sender<TonicResult<SubscribeUpdate>>,
        mut client_rx: mpsc::UnboundedReceiver<Option<Filter>>,
        mut messages_rx: broadcast::Receiver<(CommitmentLevel, GeyserMessages)>,
        drop_client: impl FnOnce(),
    ) {
        info!("client #{id}: new");

        'outer: loop {
            tokio::select! {
                message = client_rx.recv() => {
                    match message {
                        Some(Some(filter_new)) => {
                            if let Some(msg) = filter_new.get_pong_msg() {
                                if stream_tx.send(Ok(msg)).await.is_err() {
                                    error!("client #{id}: stream closed");
                                    break 'outer;
                                }
                                continue;
                            }

                            filter = filter_new;
                            info!("client #{id}: filter updated");
                        }
                        Some(None) => {
                            break 'outer;
                        },
                        None => {
                            break 'outer;
                        }
                    }
                }
                message = messages_rx.recv() => {
                    let (commitment, messages) = match message {
                        Ok((commitment, messages)) => (commitment, messages),
                        Err(broadcast::error::RecvError::Closed) => {
                            break 'outer;
                        },
                        Err(broadcast::error::RecvError::Lagged(_)) => {
                            info!("client #{id}: lagged to receive geyser messages");
                            tokio::spawn(async move {
                                let _ = stream_tx.send(Err(Status::internal("lagged"))).await;
                            });
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

        info!("client #{id}: removed");
        drop_client();
    }

    // -----------------
    // Methods/Subscription Implementations
    // -----------------
    pub(crate) async fn subscribe_impl(
        &self,
        mut request: Request<Streaming<SubscribeRequest>>,
    ) -> TonicResult<Response<ReceiverStream<TonicResult<SubscribeUpdate>>>>
    {
        let id = self.subscribe_id.fetch_add(1, Ordering::Relaxed);
        let filter = Filter::new(
            &SubscribeRequest {
                accounts: HashMap::new(),
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
        let (stream_tx, stream_rx) =
            mpsc::channel(self.config.channel_capacity);
        let (client_tx, client_rx) = mpsc::unbounded_channel();
        let notify_exit1 = Arc::new(Notify::new());
        let notify_exit2 = Arc::new(Notify::new());

        let ping_stream_tx = stream_tx.clone();
        let ping_client_tx = client_tx.clone();
        let ping_exit = Arc::clone(&notify_exit1);
        tokio::spawn(async move {
            let exit = ping_exit.notified();
            tokio::pin!(exit);

            let ping_msg = SubscribeUpdate {
                filters: vec![],
                update_oneof: Some(UpdateOneof::Ping(SubscribeUpdatePing {})),
            };

            loop {
                tokio::select! {
                    _ = &mut exit => {
                        break;
                    }
                    _ = sleep(Duration::from_secs(10)) => {
                        match ping_stream_tx.try_send(Ok(ping_msg.clone())) {
                            Ok(()) => {}
                            Err(mpsc::error::TrySendError::Full(_)) => {}
                            Err(mpsc::error::TrySendError::Closed(_)) => {
                                let _ = ping_client_tx.send(None);
                                break;
                            }
                        }
                    }
                }
            }
        });

        let config_filters_limit = self.config.filters.clone();
        let incoming_stream_tx = stream_tx.clone();
        let incoming_client_tx = client_tx;
        let incoming_exit = Arc::clone(&notify_exit2);
        let normalize_commitment_level = self.config.normalize_commitment_level;
        tokio::spawn(async move {
            let exit = incoming_exit.notified();
            tokio::pin!(exit);

            loop {
                tokio::select! {
                    _ = &mut exit => {
                        break;
                    }
                    message = request.get_mut().message() => match message {
                        Ok(Some(request)) => {
                            if let Err(error) = match Filter::new(&request, &config_filters_limit, normalize_commitment_level) {
                                Ok(filter) => match incoming_client_tx.send(Some(filter)) {
                                    Ok(()) => Ok(()),
                                    Err(error) => Err(error.to_string()),
                                },
                                Err(error) => Err(error.to_string()),
                            } {
                                let err = Err(Status::invalid_argument(format!(
                                    "failed to create filter: {error}"
                                )));
                                if incoming_stream_tx.send(err).await.is_err() {
                                    let _ = incoming_client_tx.send(None);
                                }
                            }
                        }
                        Ok(None) => {
                            break;
                        }
                        Err(_error) => {
                            let _ = incoming_client_tx.send(None);
                            break;
                        }
                    }
                }
            }
        });

        tokio::spawn(Self::client_loop(
            id,
            filter,
            stream_tx,
            client_rx,
            self.broadcast_tx.subscribe(),
            move || {
                notify_exit1.notify_one();
                notify_exit2.notify_one();
            },
        ));

        Ok(Response::new(ReceiverStream::new(stream_rx)))
    }

    async fn ping_impl(
        &self,
        request: Request<PingRequest>,
    ) -> Result<Response<PongResponse>, Status> {
        let count = request.get_ref().count;
        let response = PongResponse { count };
        Ok(Response::new(response))
    }

    async fn get_latest_blockhash_impl(
        &self,
        request: Request<GetLatestBlockhashRequest>,
    ) -> Result<Response<GetLatestBlockhashResponse>, Status> {
        if let Some(blocks_meta) = &self.blocks_meta {
            blocks_meta
                .get_block(
                    |block| {
                        block.block_height.map(|last_valid_block_height| {
                            GetLatestBlockhashResponse {
                                slot: block.slot,
                                blockhash: block.blockhash.clone(),
                                last_valid_block_height,
                            }
                        })
                    },
                    request.get_ref().commitment,
                )
                .await
        } else {
            Err(Status::unimplemented("method disabled"))
        }
    }

    async fn get_block_height_impl(
        &self,
        request: Request<GetBlockHeightRequest>,
    ) -> Result<Response<GetBlockHeightResponse>, Status> {
        if let Some(blocks_meta) = &self.blocks_meta {
            blocks_meta
                .get_block(
                    |block| {
                        block.block_height.map(|block_height| {
                            GetBlockHeightResponse { block_height }
                        })
                    },
                    request.get_ref().commitment,
                )
                .await
        } else {
            Err(Status::unimplemented("method disabled"))
        }
    }

    async fn get_slot_impl(
        &self,
        request: Request<GetSlotRequest>,
    ) -> Result<Response<GetSlotResponse>, Status> {
        if let Some(blocks_meta) = &self.blocks_meta {
            blocks_meta
                .get_block(
                    |block| Some(GetSlotResponse { slot: block.slot }),
                    request.get_ref().commitment,
                )
                .await
        } else {
            Err(Status::unimplemented("method disabled"))
        }
    }

    async fn is_blockhash_valid_impl(
        &self,
        request: Request<IsBlockhashValidRequest>,
    ) -> Result<Response<IsBlockhashValidResponse>, Status> {
        if let Some(blocks_meta) = &self.blocks_meta {
            let req = request.get_ref();
            blocks_meta
                .is_blockhash_valid(&req.blockhash, req.commitment)
                .await
        } else {
            Err(Status::unimplemented("method disabled"))
        }
    }

    async fn get_version_impl(
        &self,
        _request: Request<GetVersionRequest>,
    ) -> Result<Response<GetVersionResponse>, Status> {
        Ok(Response::new(GetVersionResponse {
            version: serde_json::to_string(&GrpcVersionInfo::default())
                .unwrap(),
        }))
    }
}

// -----------------
// Server Trait Implementation
// -----------------
#[tonic::async_trait]
impl Geyser for GrpcService {
    type SubscribeStream = ReceiverStream<TonicResult<SubscribeUpdate>>;

    async fn subscribe(
        &self,
        request: Request<Streaming<SubscribeRequest>>,
    ) -> TonicResult<Response<Self::SubscribeStream>> {
        self.subscribe_impl(request).await
    }

    async fn ping(
        &self,
        request: Request<PingRequest>,
    ) -> Result<Response<PongResponse>, Status> {
        self.ping_impl(request).await
    }

    async fn get_latest_blockhash(
        &self,
        request: Request<GetLatestBlockhashRequest>,
    ) -> Result<Response<GetLatestBlockhashResponse>, Status> {
        self.get_latest_blockhash_impl(request).await
    }

    async fn get_block_height(
        &self,
        request: Request<GetBlockHeightRequest>,
    ) -> Result<Response<GetBlockHeightResponse>, Status> {
        self.get_block_height_impl(request).await
    }

    async fn get_slot(
        &self,
        request: Request<GetSlotRequest>,
    ) -> Result<Response<GetSlotResponse>, Status> {
        self.get_slot_impl(request).await
    }

    async fn is_blockhash_valid(
        &self,
        request: Request<IsBlockhashValidRequest>,
    ) -> Result<Response<IsBlockhashValidResponse>, Status> {
        self.is_blockhash_valid_impl(request).await
    }

    async fn get_version(
        &self,
        request: Request<GetVersionRequest>,
    ) -> Result<Response<GetVersionResponse>, Status> {
        self.get_version_impl(request).await
    }
}
