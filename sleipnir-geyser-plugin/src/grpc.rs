// Adapted yellowstone-grpc/yellowstone-grpc-geyser/src/grpc.rs
use std::{
    collections::{BTreeMap, HashMap},
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
    time::{sleep, Duration, Instant},
};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{
    codec::CompressionEncoding,
    transport::server::{Server, TcpIncoming},
    Request, Response, Result as TonicResult, Status, Streaming,
};
use tonic_health::server::health_reporter;

use crate::{
    config::{ConfigBlockFailAction, ConfigGrpc},
    filters::Filter,
    grpc_messages::*,
    types::{
        geyser_message_channel, GeyserMessageReceiver, GeyserMessageSender,
        GeyserMessages,
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
    pub async fn create(
        config: ConfigGrpc,
        block_fail_action: ConfigBlockFailAction,
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
        let (blocks_meta, blocks_meta_tx) = if config.unary_disabled {
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
            blocks_meta_tx,
            broadcast_tx,
            block_fail_action,
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
        mut messages_rx: GeyserMessageReceiver,
        blocks_meta_tx: Option<GeyserMessageSender>,
        broadcast_tx: broadcast::Sender<(CommitmentLevel, GeyserMessages)>,
        block_fail_action: ConfigBlockFailAction,
    ) {
        const PROCESSED_MESSAGES_MAX: usize = 31;
        // TODO(thlorenz): @@@ This could become a bottleneck affecting latency
        const PROCESSED_MESSAGES_SLEEP: Duration = Duration::from_millis(10);

        let mut messages: BTreeMap<u64, SlotMessages> = Default::default();
        let mut processed_messages = Vec::with_capacity(PROCESSED_MESSAGES_MAX);
        let mut processed_first_slot = None;
        let processed_sleep = sleep(PROCESSED_MESSAGES_SLEEP);
        tokio::pin!(processed_sleep);

        loop {
            tokio::select! {
                Some(message) = messages_rx.recv() => {
                    // Update blocks info
                    if let Some(blocks_meta_tx) = &blocks_meta_tx {
                        if matches!(*message, Message::Slot(_) | Message::BlockMeta(_)) {
                            let _ = blocks_meta_tx.send(message.clone());
                        }
                    }

                    // Remove outdated block reconstruction info
                    match *message {
                        Message::Slot(msg) if processed_first_slot.is_none() && msg.status == CommitmentLevel::Processed => {
                            processed_first_slot = Some(msg.slot);
                        }
                        // NOTE: this used to guard to `CommitmentLevel::Processed`, but we never
                        // send that
                        Message::Slot(msg) if msg.status == CommitmentLevel::Processed => {
                            // NOTE: originally 10 slots were kept here, but we about 80x as many
                            // slots/sec
                            if let Some(msg_slot) = msg.slot.checked_sub(80) {
                                loop {
                                    match messages.keys().next().cloned() {
                                        Some(slot) if slot < msg_slot => {
                                            if let Some(slot_messages) = messages.remove(&slot) {
                                                match processed_first_slot {
                                                    Some(processed_first) if slot <= processed_first => continue,
                                                    None => continue,
                                                    _ => {}
                                                }

                                                if !slot_messages.sealed && slot_messages.finalized_at.is_some() {
                                                    let mut reasons = vec![];
                                                    if let Some(block_meta) = slot_messages.block_meta {
                                                        let block_txn_count = block_meta.executed_transaction_count as usize;
                                                        let msg_txn_count = slot_messages.transactions.len();
                                                        if block_txn_count != msg_txn_count {
                                                            reasons.push("InvalidTxnCount");
                                                            error!("failed to reconstruct #{slot} -- tx count: {block_txn_count} vs {msg_txn_count}");
                                                        }
                                                        let block_entries_count = block_meta.entries_count as usize;
                                                        let msg_entries_count = slot_messages.entries.len();
                                                        if block_entries_count != msg_entries_count {
                                                            reasons.push("InvalidEntriesCount");
                                                            error!("failed to reconstruct #{slot} -- entries count: {block_entries_count} vs {msg_entries_count}");
                                                        }
                                                    } else {
                                                        reasons.push("NoBlockMeta");
                                                    }
                                                    let reason = reasons.join(",");

                                                    match block_fail_action {
                                                        ConfigBlockFailAction::Log => {
                                                            error!("failed reconstruct #{slot} {reason}");
                                                        }
                                                        ConfigBlockFailAction::Panic => {
                                                            panic!("failed reconstruct #{slot} {reason}");
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                        _ => break,
                                    }
                                }
                            }
                        }
                        _  => {}
                    }

                    // Update block reconstruction info
                    let slot_messages = messages.entry(message.get_slot()).or_default();

                    // Runs for all messages that aren't slot updates

                    if !matches!(*message, Message::Slot(_)) {
                        // Adds 8MB / 2secs
                        slot_messages.messages.push(Some(message.clone()));

                        // If we already build Block message, new message will be a problem
                        if slot_messages.sealed && !(matches!(*message, Message::Entry(_)) && slot_messages.entries_count == 0) {
                            match block_fail_action {
                                ConfigBlockFailAction::Log => {
                                    error!("unexpected message #{} -- {} (invalid order)", message.get_slot(), message.kind());
                                }
                                ConfigBlockFailAction::Panic => {
                                    panic!("unexpected message #{} -- {} (invalid order)", message.get_slot(), message.kind());
                                }
                            }
                        }
                    }

                    let mut sealed_block_msg = None;
                    match message.as_ref() {
                        Message::BlockMeta(msg) => {
                            if slot_messages.block_meta.is_some() {
                                match block_fail_action {
                                    ConfigBlockFailAction::Log => {
                                        error!("unexpected message #{} -- BlockMeta (duplicate)", message.get_slot());
                                    }
                                    ConfigBlockFailAction::Panic => {
                                        panic!("unexpected message #{} -- BlockMeta (duplicate)", message.get_slot());
                                    }
                                }
                            }
                            slot_messages.block_meta = Some(msg.clone());
                            sealed_block_msg = slot_messages.try_seal();
                        }
                        Message::Transaction(msg) => {
                            slot_messages.transactions.push(msg.transaction.clone());
                            sealed_block_msg = slot_messages.try_seal();
                        }
                        // Dedup accounts by max write_version
                        Message::Account(msg) => {
                            let write_version = msg.account.write_version;
                            let msg_index = slot_messages.messages.len() - 1;
                            if let Some(entry) = slot_messages.accounts_dedup.get_mut(&msg.account.pubkey) {
                                if entry.0 < write_version {
                                    // We can replace the message, but in this case we will lose the order
                                    slot_messages.messages[entry.1] = None;
                                    *entry = (write_version, msg_index);
                                }
                            } else {
                                slot_messages.accounts_dedup.insert(msg.account.pubkey, (write_version, msg_index));
                            }
                        }
                        Message::Entry(msg) => {
                            slot_messages.entries.push(msg.clone());
                            sealed_block_msg = slot_messages.try_seal();
                        }
                        _ => {}
                    }

                    // Send messages to filter (and to clients)
                    let mut messages_vec = vec![message];
                    if let Some(sealed_block_msg) = sealed_block_msg {
                        messages_vec.push(sealed_block_msg);
                    }

                    for message in messages_vec {
                        if let Message::Slot(slot) = *message {
                            let (mut confirmed_messages, mut finalized_messages) = match slot.status {
                                CommitmentLevel::Processed => {
                                    (Vec::with_capacity(1), Vec::with_capacity(1))
                                }
                                CommitmentLevel::Confirmed => {
                                    if let Some(slot_messages) = messages.get_mut(&slot.slot) {
                                        if !slot_messages.sealed {
                                            slot_messages.confirmed_at = Some(slot_messages.messages.len());
                                        }
                                    }

                                    let vec = messages
                                        .get(&slot.slot)
                                        .map(|slot_messages| slot_messages.messages.iter().flatten().cloned().collect())
                                        .unwrap_or_default();
                                    (vec, Vec::with_capacity(1))
                                }
                                CommitmentLevel::Finalized => {
                                    if let Some(slot_messages) = messages.get_mut(&slot.slot) {
                                        if !slot_messages.sealed {
                                            slot_messages.finalized_at = Some(slot_messages.messages.len());
                                        }
                                    }

                                    let vec = messages
                                        .get_mut(&slot.slot)
                                        .map(|slot_messages| slot_messages.messages.iter().flatten().cloned().collect())
                                        .unwrap_or_default();
                                    (Vec::with_capacity(1), vec)
                                }
                            };

                            // processed
                            processed_messages.push(message.clone());
                            let _ =
                                broadcast_tx.send((CommitmentLevel::Processed, processed_messages.into()));
                            processed_messages = Vec::with_capacity(PROCESSED_MESSAGES_MAX);
                            processed_sleep
                                .as_mut()
                                .reset(Instant::now() + PROCESSED_MESSAGES_SLEEP);

                            // confirmed
                            confirmed_messages.push(message.clone());
                            let _ =
                                broadcast_tx.send((CommitmentLevel::Confirmed, confirmed_messages.into()));

                            // finalized
                            finalized_messages.push(message);
                            let _ =
                                broadcast_tx.send((CommitmentLevel::Finalized, finalized_messages.into()));
                        } else {
                            let mut confirmed_messages = vec![];
                            let mut finalized_messages = vec![];
                            if matches!(*message, Message::Block(_)) {
                                if let Some(slot_messages) = messages.get(&message.get_slot()) {
                                    if let Some(confirmed_at) = slot_messages.confirmed_at {
                                        confirmed_messages.extend(
                                            slot_messages.messages.as_slice()[confirmed_at..].iter().filter_map(|x| x.clone())
                                        );
                                    }
                                    if let Some(finalized_at) = slot_messages.finalized_at {
                                        finalized_messages.extend(
                                            slot_messages.messages.as_slice()[finalized_at..].iter().filter_map(|x| x.clone())
                                        );
                                    }
                                }
                            }

                            processed_messages.push(message);
                            if processed_messages.len() >= PROCESSED_MESSAGES_MAX
                                || !confirmed_messages.is_empty()
                                || !finalized_messages.is_empty()
                            {
                                let _ = broadcast_tx
                                    .send((CommitmentLevel::Processed, processed_messages.into()));
                                processed_messages = Vec::with_capacity(PROCESSED_MESSAGES_MAX);
                                processed_sleep
                                    .as_mut()
                                    .reset(Instant::now() + PROCESSED_MESSAGES_SLEEP);
                            }

                            if !confirmed_messages.is_empty() {
                                let _ =
                                    broadcast_tx.send((CommitmentLevel::Confirmed, confirmed_messages.into()));
                            }

                            if !finalized_messages.is_empty() {
                                let _ =
                                    broadcast_tx.send((CommitmentLevel::Finalized, finalized_messages.into()));
                            }
                        }
                    }
                },
                () = &mut processed_sleep => {
                    if !processed_messages.is_empty() {
                        let _ = broadcast_tx.send((CommitmentLevel::Processed, processed_messages.into()));
                        processed_messages = Vec::with_capacity(PROCESSED_MESSAGES_MAX);
                    }
                    processed_sleep.as_mut().reset(Instant::now() + PROCESSED_MESSAGES_SLEEP);
                }
                else => break,
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
