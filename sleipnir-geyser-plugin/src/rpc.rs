use std::sync::Arc;

use tokio::sync::{broadcast, mpsc, Notify};

use crate::{
    config::{ConfigBlockFailAction, ConfigGrpc},
    grpc::GrpcService,
    grpc_messages::{BlockMetaStorage, Message},
};

#[derive(Debug)]
pub struct RpcService {
    grpc_service: GrpcService,
}

impl RpcService {
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
            broadcast_tx,
            block_fail_action,
        ));

        // Run Server
        let shutdown = Arc::new(Notify::new());
        let shutdown_grpc = Arc::clone(&shutdown);
        // TODO: create Server

        Ok((messages_tx, shutdown, rpc_service))
    }
}
