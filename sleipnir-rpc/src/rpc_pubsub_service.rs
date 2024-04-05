use log::*;
use std::net::SocketAddr;

use tokio::io;

pub const MAX_ACTIVE_SUBSCRIPTIONS: usize = 1_000_000;
pub const DEFAULT_QUEUE_CAPACITY_ITEMS: usize = 10_000_000;
pub const DEFAULT_QUEUE_CAPACITY_BYTES: usize = 256 * 1024 * 1024;
pub const DEFAULT_WORKER_THREADS: usize = 1;

// -----------------
// PubSubConfig
// -----------------
#[derive(Debug, Clone)]
pub struct PubSubConfig {
    pub enable_block_subscription: bool,
    pub enable_vote_subscription: bool,
    pub max_active_subscriptions: usize,
    pub queue_capacity_items: usize,
    pub queue_capacity_bytes: usize,
    pub worker_threads: usize,
}

impl Default for PubSubConfig {
    fn default() -> Self {
        Self {
            enable_block_subscription: false,
            enable_vote_subscription: false,
            max_active_subscriptions: MAX_ACTIVE_SUBSCRIPTIONS,
            queue_capacity_items: DEFAULT_QUEUE_CAPACITY_ITEMS,
            queue_capacity_bytes: DEFAULT_QUEUE_CAPACITY_BYTES,
            worker_threads: DEFAULT_WORKER_THREADS,
        }
    }
}

pub struct PubSubService {
    config: PubSubConfig,
}

impl PubSubService {
    pub fn new(config: PubSubConfig) -> Self {
        Self { config }
    }

    pub async fn listen(&self, listen_address: SocketAddr) -> io::Result<()> {
        let listener = tokio::net::TcpListener::bind(&listen_address).await?;
        loop {
            eprintln!("Waiting for connection...");
            tokio::select! {
                result = listener.accept() => match result {
                    Ok((socket, addr)) => {
                        info!("Accepted connection from: {:?}", addr);
                    }
                    Err(e) => error!("couldn't accept connection: {:?}", e),
                }
                // TODO: tripwire
            }
        }
    }
}
