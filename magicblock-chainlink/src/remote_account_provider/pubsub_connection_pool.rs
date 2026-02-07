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

use super::errors::RemoteAccountProviderResult;
use super::pubsub_connection::{
    ProgramSubscribeResult, PubsubConnection, SubscribeResult, UnsubscribeFn,
};

/// A slot in the connection pool, wrapping a PubSubConnection and
/// tracking its subscription count.
struct PooledConnection<T: PubsubConnection> {
    connection: Arc<T>,
    sub_count: Arc<AtomicUsize>,
}

impl<T: PubsubConnection> Clone for PooledConnection<T> {
    fn clone(&self) -> Self {
        Self {
            connection: Arc::clone(&self.connection),
            sub_count: Arc::clone(&self.sub_count),
        }
    }
}

/// A pool of PubSubConnections that distributes subscriptions across
/// multiple websocket connections to stay within per-stream subscription
/// limits.
pub struct PubSubConnectionPool<T: PubsubConnection> {
    connections: Arc<Queue<PooledConnection<T>>>,
    url: String,
    per_connection_sub_limit: usize,
}

impl<T: PubsubConnection> PubSubConnectionPool<T> {
    /// Creates a new pool with a single initial connection.
    pub async fn new(
        url: String,
        limit: usize,
    ) -> RemoteAccountProviderResult<PubSubConnectionPool<T>> {
        // Creating initial connection also to verify that provider is valid
        let connection = Arc::new(T::new(url.clone()).await?);
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
    ) -> RemoteAccountProviderResult<(Arc<AtomicUsize>, Arc<T>)> {
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
        let new_connection = Arc::new(T::new(self.url.clone()).await?);

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
    ) -> Option<&'a PooledConnection<T>> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote_account_provider::pubsub_connection::mock::MockPubsubConnection;
    use solana_pubkey::Pubkey;

    fn get_connection_at_index<T: PubsubConnection>(
        pool: &PubSubConnectionPool<T>,
        index: usize,
    ) -> Option<PooledConnection<T>> {
        let guard = Guard::new();
        let mut iter = pool.connections.iter(&guard);
        iter.nth(index).cloned()
    }

    fn assert_account_subs(
        pool: &PubSubConnectionPool<MockPubsubConnection>,
        conn_subs: &[Vec<Pubkey>],
    ) {
        for (idx, expected_subs) in conn_subs.iter().enumerate() {
            let conn = get_connection_at_index(pool, idx).unwrap();
            assert_eq!(
                conn.sub_count.load(Ordering::SeqCst),
                expected_subs.len()
            );
            for pubkey in expected_subs {
                assert!(conn.connection.account_subs().contains(pubkey));
            }
        }
    }

    async fn create_pool(
        limit: usize,
    ) -> PubSubConnectionPool<MockPubsubConnection> {
        PubSubConnectionPool::<MockPubsubConnection>::new(
            "mock://".to_string(),
            limit,
        )
        .await
        .unwrap()
    }

    async fn account_subscribe(
        pool: &PubSubConnectionPool<MockPubsubConnection>,
        pubkey: &Pubkey,
    ) -> UnsubscribeFn {
        let (_stream, unsub) = pool
            .account_subscribe(pubkey, RpcAccountInfoConfig::default())
            .await
            .unwrap();
        unsub
    }

    #[tokio::test]
    async fn test_single_sub() {
        let pool = create_pool(2).await;
        let pk1 = Pubkey::new_unique();

        let _unsub1 = account_subscribe(&pool, &pk1).await;

        assert_account_subs(&pool, &[vec![pk1]]);
    }

    #[tokio::test]
    async fn test_two_subs_one_connection() {
        let pool = create_pool(2).await;
        let pk1 = Pubkey::new_unique();
        let pk2 = Pubkey::new_unique();

        let _unsub1 = account_subscribe(&pool, &pk1).await;
        let _unsub2 = account_subscribe(&pool, &pk2).await;

        assert_account_subs(&pool, &[vec![pk1, pk2]]);
    }

    #[tokio::test]
    async fn test_three_subs_two_connections() {
        let pool = create_pool(2).await;
        let pk1 = Pubkey::new_unique();
        let pk2 = Pubkey::new_unique();
        let pk3 = Pubkey::new_unique();

        let _unsub1 = account_subscribe(&pool, &pk1).await;
        let _unsub2 = account_subscribe(&pool, &pk2).await;
        let _unsub3 = account_subscribe(&pool, &pk3).await;

        assert_account_subs(&pool, &[vec![pk1, pk2], vec![pk3]]);
    }

    #[tokio::test]
    async fn test_unsub_frees_slot_and_new_sub_fills_it() {
        let pool = create_pool(2).await;
        let pk1 = Pubkey::new_unique();
        let pk2 = Pubkey::new_unique();
        let pk3 = Pubkey::new_unique();
        let pk4 = Pubkey::new_unique();
        let pk5 = Pubkey::new_unique();

        // Fill 2 connections: [pk1, pk2] and [pk3, pk4]
        let _unsub1 = account_subscribe(&pool, &pk1).await;
        let unsub2 = account_subscribe(&pool, &pk2).await;
        let _unsub3 = account_subscribe(&pool, &pk3).await;
        let _unsub4 = account_subscribe(&pool, &pk4).await;

        assert_account_subs(&pool, &[vec![pk1, pk2], vec![pk3, pk4]]);

        // Unsubscribe pk2 from connection 0, freeing a slot
        unsub2().await;

        // New sub should go to connection 0 (first with capacity)
        let _unsub5 = account_subscribe(&pool, &pk5).await;

        // conn0 now has pk1+pk5 (sub_count=2), conn1 unchanged
        assert_account_subs(
            &pool,
            &[vec![pk1, pk5], vec![pk3, pk4]],
        );
    }

    #[tokio::test]
    async fn test_elaborate_sub_unsub_lifecycle() {
        let pool = create_pool(2).await;
        let pks: Vec<Pubkey> = (0..8).map(|_| Pubkey::new_unique()).collect();

        // Sub pk0, pk1 -> conn0 full
        let unsub0 = account_subscribe(&pool, &pks[0]).await;
        let unsub1 = account_subscribe(&pool, &pks[1]).await;
        assert_account_subs(&pool, &[vec![pks[0], pks[1]]]);

        // Sub pk2 -> conn1 created
        let _unsub2 = account_subscribe(&pool, &pks[2]).await;
        assert_account_subs(&pool, &[vec![pks[0], pks[1]], vec![pks[2]]]);

        // Sub pk3 -> conn1 full
        let unsub3 = account_subscribe(&pool, &pks[3]).await;
        assert_account_subs(
            &pool,
            &[vec![pks[0], pks[1]], vec![pks[2], pks[3]]],
        );

        // Sub pk4 -> conn2 created
        let _unsub4 = account_subscribe(&pool, &pks[4]).await;
        assert_account_subs(
            &pool,
            &[vec![pks[0], pks[1]], vec![pks[2], pks[3]], vec![pks[4]]],
        );

        // Unsub pk0 from conn0 -> conn0 has capacity
        unsub0().await;

        // Sub pk5 -> goes to conn0 (first with capacity)
        let _unsub5 = account_subscribe(&pool, &pks[5]).await;
        assert_account_subs(
            &pool,
            &[
                vec![pks[1], pks[5]],
                vec![pks[2], pks[3]],
                vec![pks[4]],
            ],
        );

        // Unsub pk1, pk3 -> conn0 and conn1 each drop to 1
        unsub1().await;
        unsub3().await;

        // Sub pk6 -> fills conn0, pk7 -> fills conn1
        let _unsub6 = account_subscribe(&pool, &pks[6]).await;
        let _unsub7 = account_subscribe(&pool, &pks[7]).await;
        assert_account_subs(
            &pool,
            &[
                vec![pks[5], pks[6]],
                vec![pks[2], pks[7]],
                vec![pks[4]],
            ],
        );
    }
}