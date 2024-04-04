#![allow(unused)]
use log::*;
use std::sync::Arc;

use geyser_grpc_proto::geyser::CommitmentLevel;
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
            broadcast_tx.clone(),
            block_fail_action,
        ));

        // Run Server
        let shutdown = Arc::new(Notify::new());
        let shutdown_grpc = Arc::clone(&shutdown);
        // TODO: create Server

        let id = 0;
        tokio::spawn(Self::client_loop(id, broadcast_tx.subscribe()));

        Ok((messages_tx, shutdown, rpc_service))
    }

    async fn client_loop(
        id: usize,
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
                    debug!("RPC messages: {:?}", messages);
                }
            }
        }
    }
}
