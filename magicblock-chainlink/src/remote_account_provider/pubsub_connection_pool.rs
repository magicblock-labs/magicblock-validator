use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use scc::{ebr::Guard, Queue};
use solana_pubkey::Pubkey;
use solana_rpc_client_api::config::{
    RpcAccountInfoConfig, RpcProgramAccountsConfig,
};
use tracing::*;

use super::pubsub_connection::{
    ProgramSubscribeResult, PubsubConnection, PubsubConnectionImpl,
    SubscribeResult, UnsubscribeFn,
};
use super::errors::RemoteAccountProviderResult;

/// A slot in the connection pool, wrapping a PubSubConnection and
/// tracking its subscription count.
struct PooledConnection {
    connection: Arc<PubsubConnectionImpl>,
    sub_count: Arc<AtomicUsize>,
}

/// A pool of PubSubConnections that distributes subscriptions across
/// multiple websocket connections to stay within per-stream subscription
/// limits.
pub struct PubSubConnectionPool {
    connections: Arc<Queue<PooledConnection>>,
    url: String,
    per_connection_sub_limit: usize,
}

impl PubSubConnectionPool {
    /// Creates a new pool with a single initial connection.
    pub async fn new(
        url: String,
        limit: usize,
    ) -> RemoteAccountProviderResult<Self> {
        // Creating initial connection also to verify that provider is valid
        let connection =
            Arc::new(PubsubConnectionImpl::new(url.clone()).await?);
        let conn = PooledConnection {
            connection,
            sub_count: Arc::new(AtomicUsize::new(0)),
        };
        let queue = {
            let queue = Queue::default();
            queue.push(conn);
            queue
        };
        Ok(Self {
            connections: Arc::new(queue),
            url,
            per_connection_sub_limit: limit,
        })
    }

    /// Returns the websocket URL.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Subscribes to account updates, distributing across pool slots.
    pub async fn account_subscribe(
        &self,
        pubkey: &Pubkey,
        config: RpcAccountInfoConfig,
    ) -> SubscribeResult {
        let (sub_count, connection) =
            match self.find_or_create_connection().await {
                Ok(result) => result,
                Err(err) => return Err(err.into()),
            };

        // Subscribe using the selected connection
        match connection.account_subscribe(pubkey, config).await {
            Ok((stream, raw_unsub)) => {
                let wrapped_unsub = self.wrap_unsub(raw_unsub, sub_count);
                Ok((stream, wrapped_unsub))
            }
            Err(err) => {
                // Rollback: decrement count
                sub_count.fetch_sub(1, Ordering::SeqCst);
                Err(err)
            }
        }
    }

    /// Subscribes to program account updates, distributing across pool slots.
    pub async fn program_subscribe(
        &self,
        program_id: &Pubkey,
        config: RpcProgramAccountsConfig,
    ) -> ProgramSubscribeResult {
        let (sub_count, connection) =
            match self.find_or_create_connection().await {
                Ok(result) => result,
                Err(err) => return Err(err.into()),
            };

        // Subscribe using the selected connection
        match connection.program_subscribe(program_id, config).await {
            Ok((stream, raw_unsub)) => {
                let wrapped_unsub = self.wrap_unsub(raw_unsub, sub_count);
                Ok((stream, wrapped_unsub))
            }
            Err(err) => {
                // Rollback: decrement count
                sub_count.fetch_sub(1, Ordering::SeqCst);
                Err(err)
            }
        }
    }

    /// Reconnects the pool: clears state and reconnects the first slot.
    pub fn clear_connections(&self) {
        while self.connections.pop().is_some() {}
    }

    /// Finds a connection for a new subscription, creating new connections
    /// as needed. Returns (sub_count, connection).
    async fn find_or_create_connection(
        &self,
    ) -> RemoteAccountProviderResult<(
        Arc<AtomicUsize>,
        Arc<PubsubConnectionImpl>,
    )> {
        // Phase 1: Try to find a slot with capacity under lock

        {
            let guard = Guard::new();
            if let Some(pooled_conn) = self.pick_connection(&guard) {
                let sub_count = Arc::clone(&pooled_conn.sub_count);
                sub_count.fetch_add(1, Ordering::SeqCst);
                return Ok((sub_count, Arc::clone(&pooled_conn.connection)));
            }
        }

        // Phase 2: No slot has capacity; create new connection (async)
        let new_connection =
            Arc::new(PubsubConnectionImpl::new(self.url.clone()).await?);

        // Phase 3: Add new slot to pool under lock
        let sub_count = Arc::new(AtomicUsize::new(1));
        let conn = PooledConnection {
            connection: Arc::clone(&new_connection),
            sub_count: Arc::clone(&sub_count),
        };
        self.connections.push(conn);
        trace!("Created new pooled connection");
        Ok((sub_count, new_connection))
    }

    /// Picks a slot with available capacity using first-fit.
    /// Returns None if no slot has capacity (need to create new connection).
    fn pick_connection<'a>(
        &self,
        guard: &'a Guard,
    ) -> Option<&'a PooledConnection> {
        self.connections.iter(guard).find(|conn| {
            conn.sub_count.load(Ordering::SeqCst)
                < self.per_connection_sub_limit
        })
    }

    /// Wraps a raw unsubscribe function to also decrement the sub counter for the
    /// connection on which it was made.
    fn wrap_unsub(
        &self,
        raw_unsub: UnsubscribeFn,
        sub_count: Arc<AtomicUsize>,
    ) -> UnsubscribeFn {
        Box::new(move || {
            Box::pin(async move {
                raw_unsub().await;
                sub_count.fetch_sub(1, Ordering::SeqCst);
            })
        })
    }
}