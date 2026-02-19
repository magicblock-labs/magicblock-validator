use std::sync::{
    atomic::{AtomicU64, AtomicUsize, Ordering},
    Arc,
};

use magicblock_metrics::metrics;
use scc::{ebr::Guard, Queue};
use solana_pubkey::Pubkey;
use solana_pubsub_client::{
    nonblocking::pubsub_client::PubsubClientResult,
    pubsub_client::PubsubClientError,
};
use solana_rpc_client_api::config::{
    RpcAccountInfoConfig, RpcProgramAccountsConfig,
};
use tokio::sync::Mutex as AsyncMutex;
use tracing::*;

use super::{
    errors::RemoteAccountProviderResult,
    pubsub_connection::{
        ProgramSubscribeResult, PubsubConnection, SubscribeResult,
        UnsubscribeFn,
    },
};

/// A slot in the connection pool, wrapping a PubSubConnection and
/// tracking its subscription count.
struct PooledConnection<T: PubsubConnection> {
    connection: Arc<T>,
    sub_count: Arc<AtomicUsize>,
    id: u64,
}

impl<T: PubsubConnection> Clone for PooledConnection<T> {
    fn clone(&self) -> Self {
        Self {
            connection: Arc::clone(&self.connection),
            sub_count: Arc::clone(&self.sub_count),
            id: self.id,
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
    new_connection_guard: AsyncMutex<()>,
    client_id: String,
    next_connection_id: AtomicU64,
}

impl<T: PubsubConnection> PubSubConnectionPool<T> {
    /// Creates a new pool with a single initial connection.
    pub async fn new(
        url: String,
        limit: usize,
        client_id: String,
    ) -> RemoteAccountProviderResult<PubSubConnectionPool<T>> {
        // Creating initial connection also to verify that provider is valid
        let connection = Arc::new(T::new(url.clone()).await?);
        let conn = PooledConnection {
            connection,
            sub_count: Arc::new(AtomicUsize::new(0)),
            id: 0,
        };
        let queue = {
            let queue = Queue::default();
            queue.push(conn);
            queue
        };
        metrics::set_pubsub_client_connections_count(&client_id, 1);
        Ok(Self {
            connections: Arc::new(queue),
            url,
            per_connection_sub_limit: limit,
            new_connection_guard: AsyncMutex::new(()),
            client_id,
            next_connection_id: AtomicU64::new(1),
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
        let pubkey = *pubkey;
        self.subscribe_with_pool(|connection| async move {
            connection.account_subscribe(&pubkey, config).await
        })
        .await
    }

    /// Subscribes to program account updates, distributing across pool slots.
    pub async fn program_subscribe(
        &self,
        program_id: &Pubkey,
        config: RpcProgramAccountsConfig,
    ) -> ProgramSubscribeResult {
        let program_id = *program_id;
        self.subscribe_with_pool(|connection| async move {
            connection.program_subscribe(&program_id, config).await
        })
        .await
    }

    /// Helper to perform a subscription with a provided closure,
    /// handling find-or-create, error mapping, rollback, and unsubscribe wrapping.
    async fn subscribe_with_pool<F, Fut, S>(
        &self,
        subscribe_fn: F,
    ) -> PubsubClientResult<(S, UnsubscribeFn)>
    where
        F: FnOnce(Arc<T>) -> Fut,
        Fut: std::future::Future<
            Output = PubsubClientResult<(S, UnsubscribeFn)>,
        >,
        S: 'static,
    {
        // Find or create a connection
        let (sub_count, connection, connection_id) =
            match self.find_or_create_connection().await {
                Ok(result) => result,
                Err(err) => {
                    return Err(PubsubClientError::SubscribeFailed {
                        reason: "Unable to find or create connection"
                            .to_string(),
                        message: format!("{err:?}"),
                    });
                }
            };

        // Subscribe using the selected connection
        match subscribe_fn(connection).await {
            Ok((stream, raw_unsub)) => {
                let wrapped_unsub = self.wrap_unsub(raw_unsub, sub_count);
                Ok((stream, wrapped_unsub))
            }
            Err(err) => {
                // Rollback: decrement count
                sub_count.fetch_sub(1, Ordering::SeqCst);
                self.on_subscribe_error(connection_id, &err).await;
                Err(err)
            }
        }
    }

    /// Reconnects the pool: clears state and tries to reconnect the
    /// first connection to ensure that the provider is working
    /// NOTE: assumes that all existing subscriptions have been dropped.
    pub async fn reconnect(&self) -> PubsubClientResult<()> {
        let _new_conn_guard = self.new_connection_guard.lock().await;
        while self.connections.pop().is_some() {}
        // We cannot reconnect an existing connection due to the lockless queue
        // not allowing us to call the async reconnect method of the first connection.
        // Instead we clear all of them and then create a new one, just to verify that
        // the provider is working again
        let pooled_conn = match T::new(self.url.clone()).await {
            Ok(conn) => {
                conn.reconnect().await?;
                PooledConnection {
                    connection: Arc::new(conn),
                    sub_count: Arc::new(AtomicUsize::new(0)),
                    id: self.next_connection_id(),
                }
            }
            Err(err) => {
                return Err(PubsubClientError::ConnectionClosed(format!(
                    "{err:?}"
                )));
            }
        };
        // Since we already created it we keep it as well
        self.connections.push(pooled_conn);
        metrics::set_pubsub_client_connections_count(&self.client_id, 1);
        Ok(())
    }

    /// Finds a connection for a new subscription, creating new connections
    /// as needed. Returns (sub_count, connection).
    async fn find_or_create_connection(
        &self,
    ) -> RemoteAccountProviderResult<(Arc<AtomicUsize>, Arc<T>, u64)> {
        fn try_reserve_connection<T: PubsubConnection>(
            pool: &PubSubConnectionPool<T>,
        ) -> Option<(Arc<AtomicUsize>, Arc<T>, u64)> {
            let guard = Guard::new();
            pool.try_insert_sub(&guard)
        }

        // Phase 1: fast path — try to reserve a slot without locking
        if let Some(result) = try_reserve_connection(self) {
            return Ok(result);
        }

        // Serialize connection creation
        let _new_conn_guard = self.new_connection_guard.lock().await;

        // Phase 2: re-check under lock — another task may have
        // created a connection while we waited
        if let Some(result) = try_reserve_connection(self) {
            return Ok(result);
        }

        // Phase 3: still no capacity — create and push new connection
        let new_connection = Arc::new(T::new(self.url.clone()).await?);
        let sub_count = Arc::new(AtomicUsize::new(1));
        let connection_id = self.next_connection_id();
        let conn = PooledConnection {
            connection: Arc::clone(&new_connection),
            sub_count: Arc::clone(&sub_count),
            id: connection_id,
        };
        self.connections.push(conn);
        let connection_count = self.connections.len();
        metrics::set_pubsub_client_connections_count(
            &self.client_id,
            connection_count,
        );
        trace!(
            url = self.url,
            connection_count,
            "Created new pooled connection"
        );
        Ok((sub_count, new_connection, connection_id))
    }

    /// Tries to atomically reserve a subscription slot on an existing
    /// connection via CAS, ensuring we never exceed
    /// `per_connection_sub_limit`.
    fn try_insert_sub(
        &self,
        guard: &Guard,
    ) -> Option<(Arc<AtomicUsize>, Arc<T>, u64)> {
        for conn in self.connections.iter(guard) {
            let sub_count = &conn.sub_count;
            loop {
                let current = sub_count.load(Ordering::SeqCst);
                if current >= self.per_connection_sub_limit {
                    break;
                }
                if sub_count
                    .compare_exchange(
                        current,
                        current + 1,
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    )
                    .is_ok()
                {
                    return Some((
                        Arc::clone(&conn.sub_count),
                        Arc::clone(&conn.connection),
                        conn.id,
                    ));
                }
            }
        }
        None
    }

    fn next_connection_id(&self) -> u64 {
        self.next_connection_id.fetch_add(1, Ordering::SeqCst)
    }

    pub(crate) fn is_connection_level_error(err: &PubsubClientError) -> bool {
        matches!(
            err,
            PubsubClientError::ConnectionError(_)
                | PubsubClientError::WsError(_)
                | PubsubClientError::ConnectionClosed(_)
        )
    }

    async fn on_subscribe_error(
        &self,
        connection_id: u64,
        err: &PubsubClientError,
    ) {
        if !Self::is_connection_level_error(err) {
            return;
        }

        let removed = self.remove_connection_by_id(connection_id).await;
        if removed.is_some() {
            let connection_count = self.connections.len();
            metrics::set_pubsub_client_connections_count(
                &self.client_id,
                connection_count,
            );
            warn!(
                client_id = self.client_id,
                connection_id,
                connection_count,
                error = ?err,
                "Removed failing pooled connection after subscribe error"
            );
        }
    }

    async fn remove_connection_by_id(
        &self,
        connection_id: u64,
    ) -> Option<PooledConnection<T>> {
        // Serialize with connection creation/reconnect to avoid queue rebuild races.
        let _new_conn_guard = self.new_connection_guard.lock().await;
        let mut removed = None;
        let mut retained = Vec::new();

        while let Some(conn_entry) = self.connections.pop() {
            let conn = (**conn_entry).clone();
            if conn.id == connection_id {
                removed = Some(conn);
            } else {
                retained.push(conn);
            }
        }

        for conn in retained {
            self.connections.push(conn);
        }

        removed
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
    use std::{
        collections::HashMap,
        sync::{Mutex, OnceLock},
    };

    use async_trait::async_trait;
    use solana_pubkey::Pubkey;
    use solana_pubsub_client::pubsub_client::PubsubClientError;

    use super::*;
    use crate::remote_account_provider::pubsub_connection::mock::MockPubsubConnection;

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
            "test_client".to_string(),
        )
        .await
        .unwrap()
    }

    #[derive(Clone)]
    struct FlakyByOrdinalConnection {
        ordinal: u64,
        account_subscriptions: Arc<Mutex<Vec<Pubkey>>>,
    }

    impl FlakyByOrdinalConnection {
        fn next_ordinal(url: &str) -> u64 {
            static URL_CONNECTION_ORDINALS: OnceLock<
                Mutex<HashMap<String, u64>>,
            > = OnceLock::new();
            let ordinals = URL_CONNECTION_ORDINALS
                .get_or_init(|| Mutex::new(HashMap::new()));
            let mut ordinals = ordinals.lock().unwrap();
            let entry = ordinals.entry(url.to_string()).or_insert(0);
            let current = *entry;
            *entry += 1;
            current
        }

        fn account_subs(&self) -> Vec<Pubkey> {
            self.account_subscriptions.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl PubsubConnection for FlakyByOrdinalConnection {
        async fn new(url: String) -> RemoteAccountProviderResult<Self>
        where
            Self: Sized,
        {
            Ok(Self {
                ordinal: Self::next_ordinal(&url),
                account_subscriptions: Arc::new(Mutex::new(Vec::new())),
            })
        }

        fn url(&self) -> &str {
            "mock://flaky"
        }

        async fn account_subscribe(
            &self,
            pubkey: &Pubkey,
            _config: RpcAccountInfoConfig,
        ) -> SubscribeResult {
            if self.ordinal == 1 {
                return Err(PubsubClientError::ConnectionClosed(
                    "simulated failing connection".to_string(),
                ));
            }
            self.account_subscriptions.lock().unwrap().push(*pubkey);
            let stream = Box::pin(futures_util::stream::empty());
            let unsubscribe: UnsubscribeFn = Box::new(|| Box::pin(async {}));
            Ok((stream, unsubscribe))
        }

        async fn program_subscribe(
            &self,
            _program_id: &Pubkey,
            _config: RpcProgramAccountsConfig,
        ) -> ProgramSubscribeResult {
            let stream = Box::pin(futures_util::stream::empty());
            let unsubscribe: UnsubscribeFn = Box::new(|| Box::pin(async {}));
            Ok((stream, unsubscribe))
        }

        async fn reconnect(&self) -> PubsubClientResult<()> {
            Ok(())
        }
    }

    #[derive(Clone)]
    struct LogicalErrorByOrdinalConnection {
        ordinal: u64,
    }

    impl LogicalErrorByOrdinalConnection {
        fn next_ordinal(url: &str) -> u64 {
            static URL_CONNECTION_ORDINALS: OnceLock<
                Mutex<HashMap<String, u64>>,
            > = OnceLock::new();
            let ordinals = URL_CONNECTION_ORDINALS
                .get_or_init(|| Mutex::new(HashMap::new()));
            let mut ordinals = ordinals.lock().unwrap();
            let entry = ordinals.entry(url.to_string()).or_insert(0);
            let current = *entry;
            *entry += 1;
            current
        }
    }

    #[async_trait]
    impl PubsubConnection for LogicalErrorByOrdinalConnection {
        async fn new(url: String) -> RemoteAccountProviderResult<Self>
        where
            Self: Sized,
        {
            Ok(Self {
                ordinal: Self::next_ordinal(&url),
            })
        }

        fn url(&self) -> &str {
            "mock://logical"
        }

        async fn account_subscribe(
            &self,
            _pubkey: &Pubkey,
            _config: RpcAccountInfoConfig,
        ) -> SubscribeResult {
            if self.ordinal == 1 {
                return Err(PubsubClientError::SubscribeFailed {
                    reason: "bad request".to_string(),
                    message: "simulated logical failure".to_string(),
                });
            }
            let stream = Box::pin(futures_util::stream::empty());
            let unsubscribe: UnsubscribeFn = Box::new(|| Box::pin(async {}));
            Ok((stream, unsubscribe))
        }

        async fn program_subscribe(
            &self,
            _program_id: &Pubkey,
            _config: RpcProgramAccountsConfig,
        ) -> ProgramSubscribeResult {
            let stream = Box::pin(futures_util::stream::empty());
            let unsubscribe: UnsubscribeFn = Box::new(|| Box::pin(async {}));
            Ok((stream, unsubscribe))
        }

        async fn reconnect(&self) -> PubsubClientResult<()> {
            Ok(())
        }
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

    fn assert_program_subs(
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
                assert!(conn.connection.program_subs().contains(pubkey));
            }
        }
    }

    async fn program_subscribe(
        pool: &PubSubConnectionPool<MockPubsubConnection>,
        program_id: &Pubkey,
    ) -> UnsubscribeFn {
        let (_stream, unsub) = pool
            .program_subscribe(program_id, RpcProgramAccountsConfig::default())
            .await
            .unwrap();
        unsub
    }

    fn create_pubkeys<const N: usize>() -> [Pubkey; N] {
        (0..N)
            .map(|_| Pubkey::new_unique())
            .collect::<Vec<_>>()
            .try_into()
            .unwrap()
    }

    #[tokio::test]
    async fn test_single_sub() {
        let pool = create_pool(2).await;
        let pk1 = Pubkey::new_unique();

        // Sub account(pk1) -> Conn0 (1/2)
        let _unsub1 = account_subscribe(&pool, &pk1).await;
        // Final: Conn0 (pk1)
        assert_account_subs(&pool, &[vec![pk1]]);
    }

    #[tokio::test]
    async fn test_two_subs_one_connection() {
        let pool = create_pool(2).await;
        let [pk1, pk2] = create_pubkeys();

        // Sub account(pk1) -> Conn0 (1/2)
        let _unsub1 = account_subscribe(&pool, &pk1).await;
        // Sub account(pk2) -> Conn0 (2/2 FULL)
        let _unsub2 = account_subscribe(&pool, &pk2).await;
        // Final: Conn0 (pk1, pk2)
        assert_account_subs(&pool, &[vec![pk1, pk2]]);
    }

    #[tokio::test]
    async fn test_three_subs_two_connections() {
        let pool = create_pool(2).await;
        let [pk1, pk2, pk3] = create_pubkeys();

        // Sub account(pk1) -> Conn0 (1/2)
        let _unsub1 = account_subscribe(&pool, &pk1).await;
        // Sub account(pk2) -> Conn0 (2/2 FULL)
        let _unsub2 = account_subscribe(&pool, &pk2).await;
        // Sub account(pk3) -> Conn1 created (1/2) [Conn0 is full]
        let _unsub3 = account_subscribe(&pool, &pk3).await;
        // Final: Conn0 (pk1, pk2), Conn1 (pk3)
        assert_account_subs(&pool, &[vec![pk1, pk2], vec![pk3]]);
    }

    #[tokio::test]
    async fn test_unsub_frees_slot_and_new_sub_fills_it() {
        let pool = create_pool(2).await;
        let [pk1, pk2, pk3, pk4, pk5] = create_pubkeys();

        // Fill Conn0 with pk1, pk2
        let _unsub1 = account_subscribe(&pool, &pk1).await;
        let unsub2 = account_subscribe(&pool, &pk2).await;
        // Create Conn1, fill with pk3, pk4
        let _unsub3 = account_subscribe(&pool, &pk3).await;
        let _unsub4 = account_subscribe(&pool, &pk4).await;
        assert_account_subs(&pool, &[vec![pk1, pk2], vec![pk3, pk4]]);

        // Unsub pk2 from Conn0, freeing a slot
        unsub2().await;

        // Sub pk5 goes to Conn0 (first-fit)
        let _unsub5 = account_subscribe(&pool, &pk5).await;
        // Final: Conn0 (pk1, pk5), Conn1 (pk3, pk4)
        assert_account_subs(&pool, &[vec![pk1, pk5], vec![pk3, pk4]]);
    }

    #[tokio::test]
    async fn test_elaborate_sub_unsub_lifecycle() {
        // Complex lifecycle: sub/unsub across 3 connections
        let pool = create_pool(2).await;
        let pks = create_pubkeys::<8>();

        // Sub pk0, pk1 -> Conn0 full
        let unsub0 = account_subscribe(&pool, &pks[0]).await;
        let unsub1 = account_subscribe(&pool, &pks[1]).await;
        assert_account_subs(&pool, &[vec![pks[0], pks[1]]]);

        // Sub pk2 -> Conn1 created
        let _unsub2 = account_subscribe(&pool, &pks[2]).await;
        assert_account_subs(&pool, &[vec![pks[0], pks[1]], vec![pks[2]]]);

        // Sub pk3 -> Conn1 full
        let unsub3 = account_subscribe(&pool, &pks[3]).await;
        assert_account_subs(
            &pool,
            &[vec![pks[0], pks[1]], vec![pks[2], pks[3]]],
        );

        // Sub pk4 -> Conn2 created
        let _unsub4 = account_subscribe(&pool, &pks[4]).await;
        assert_account_subs(
            &pool,
            &[vec![pks[0], pks[1]], vec![pks[2], pks[3]], vec![pks[4]]],
        );

        // Unsub pk0 from Conn0 -> Conn0 has capacity
        unsub0().await;

        // Sub pk5 -> goes to Conn0 (first-fit)
        let _unsub5 = account_subscribe(&pool, &pks[5]).await;
        assert_account_subs(
            &pool,
            &[vec![pks[1], pks[5]], vec![pks[2], pks[3]], vec![pks[4]]],
        );

        // Unsub pk1, pk3 -> Conn0 and Conn1 each drop to 1
        unsub1().await;
        unsub3().await;

        // Sub pk6 -> fills Conn0, pk7 -> fills Conn1
        let _unsub6 = account_subscribe(&pool, &pks[6]).await;
        let _unsub7 = account_subscribe(&pool, &pks[7]).await;
        // Final: Conn0 (pk5, pk6), Conn1 (pk2, pk7), Conn2 (pk4)
        assert_account_subs(
            &pool,
            &[vec![pks[5], pks[6]], vec![pks[2], pks[7]], vec![pks[4]]],
        );
    }

    #[tokio::test]
    async fn test_program_single_sub() {
        let pool = create_pool(2).await;
        let pid1 = Pubkey::new_unique();

        // Sub program(pid1) -> Conn0 (1/2)
        let _unsub1 = program_subscribe(&pool, &pid1).await;
        // Final: Conn0 (pid1)
        assert_program_subs(&pool, &[vec![pid1]]);
    }

    #[tokio::test]
    async fn test_program_two_subs_one_connection() {
        let pool = create_pool(2).await;
        let [pid1, pid2] = create_pubkeys();

        // Sub program(pid1) -> Conn0 (1/2)
        let _unsub1 = program_subscribe(&pool, &pid1).await;
        // Sub program(pid2) -> Conn0 (2/2 FULL)
        let _unsub2 = program_subscribe(&pool, &pid2).await;
        // Final: Conn0 (pid1, pid2)
        assert_program_subs(&pool, &[vec![pid1, pid2]]);
    }

    #[tokio::test]
    async fn test_program_three_subs_two_connections() {
        let pool = create_pool(2).await;
        let [pid1, pid2, pid3] = create_pubkeys();

        // Sub program(pid1) -> Conn0 (1/2)
        let _unsub1 = program_subscribe(&pool, &pid1).await;
        // Sub program(pid2) -> Conn0 (2/2 FULL)
        let _unsub2 = program_subscribe(&pool, &pid2).await;
        // Sub program(pid3) -> Conn1 created (1/2) [Conn0 is full]
        let _unsub3 = program_subscribe(&pool, &pid3).await;
        // Final: Conn0 (pid1, pid2), Conn1 (pid3)
        assert_program_subs(&pool, &[vec![pid1, pid2], vec![pid3]]);
    }

    #[tokio::test]
    async fn test_program_unsub_frees_slot_and_new_sub_fills_it() {
        let pool = create_pool(2).await;
        let [pid1, pid2, pid3, pid4, pid5] = create_pubkeys();

        // Fill Conn0 with pid1, pid2
        let _unsub1 = program_subscribe(&pool, &pid1).await;
        let unsub2 = program_subscribe(&pool, &pid2).await;
        // Create Conn1, fill with pid3, pid4
        let _unsub3 = program_subscribe(&pool, &pid3).await;
        let _unsub4 = program_subscribe(&pool, &pid4).await;
        assert_program_subs(&pool, &[vec![pid1, pid2], vec![pid3, pid4]]);

        // Unsub pid2 from Conn0, freeing a slot
        unsub2().await;

        // Sub pid5 goes to Conn0 (first-fit)
        let _unsub5 = program_subscribe(&pool, &pid5).await;
        // Final: Conn0 (pid1, pid5), Conn1 (pid3, pid4)
        assert_program_subs(&pool, &[vec![pid1, pid5], vec![pid3, pid4]]);
    }

    fn assert_mixed_subs(
        pool: &PubSubConnectionPool<MockPubsubConnection>,
        conn_subs: &[(Vec<Pubkey>, Vec<Pubkey>)],
    ) {
        for (idx, (expected_account, expected_program)) in
            conn_subs.iter().enumerate()
        {
            let conn = get_connection_at_index(pool, idx).unwrap();
            let expected_total =
                expected_account.len() + expected_program.len();
            assert_eq!(conn.sub_count.load(Ordering::SeqCst), expected_total);
            for pubkey in expected_account {
                assert!(conn.connection.account_subs().contains(pubkey));
            }
            for pubkey in expected_program {
                assert!(conn.connection.program_subs().contains(pubkey));
            }
        }
    }

    #[tokio::test]
    async fn test_mixed_subs_respect_limit() {
        // Accounts and programs both count toward the per-connection limit
        let pool = create_pool(2).await;
        let ak1 = Pubkey::new_unique();
        let pk1 = Pubkey::new_unique();
        let ak2 = Pubkey::new_unique();

        // Sub account(ak1) -> Conn0 (1/2)
        let _unsub_a1 = account_subscribe(&pool, &ak1).await;
        // Sub program(pk1) -> Conn0 (2/2 FULL)
        let _unsub_p1 = program_subscribe(&pool, &pk1).await;
        // Final: Conn0 (ak1 + pk1)
        assert_mixed_subs(&pool, &[(vec![ak1], vec![pk1])]);

        // Sub account(ak2) -> Conn1 created (1/2) [Conn0 is full]
        let _unsub_a2 = account_subscribe(&pool, &ak2).await;
        // Final: Conn0 (ak1 + pk1), Conn1 (ak2)
        assert_mixed_subs(
            &pool,
            &[(vec![ak1], vec![pk1]), (vec![ak2], vec![])],
        );
    }

    #[tokio::test]
    async fn test_mixed_elaborate_sub_unsub_lifecycle() {
        // Complex mixed lifecycle: accounts and programs inter-mixed across 3 connections
        let pool = create_pool(2).await;
        let aks = create_pubkeys::<4>();
        let pks = create_pubkeys::<4>();

        // Account + program on Conn0 -> full
        let unsub_a0 = account_subscribe(&pool, &aks[0]).await;
        let unsub_p0 = program_subscribe(&pool, &pks[0]).await;
        assert_mixed_subs(&pool, &[(vec![aks[0]], vec![pks[0]])]);

        // Next account -> Conn1 created
        let _unsub_a1 = account_subscribe(&pool, &aks[1]).await;
        assert_mixed_subs(
            &pool,
            &[(vec![aks[0]], vec![pks[0]]), (vec![aks[1]], vec![])],
        );

        // Next program -> Conn1 full
        let unsub_p1 = program_subscribe(&pool, &pks[1]).await;
        assert_mixed_subs(
            &pool,
            &[(vec![aks[0]], vec![pks[0]]), (vec![aks[1]], vec![pks[1]])],
        );

        // Next account -> Conn2 created
        let _unsub_a2 = account_subscribe(&pool, &aks[2]).await;
        assert_mixed_subs(
            &pool,
            &[
                (vec![aks[0]], vec![pks[0]]),
                (vec![aks[1]], vec![pks[1]]),
                (vec![aks[2]], vec![]),
            ],
        );

        // Unsub account from Conn0 -> Conn0 has capacity
        unsub_a0().await;

        // Next program -> goes to Conn0 (first-fit)
        let _unsub_p2 = program_subscribe(&pool, &pks[2]).await;
        assert_mixed_subs(
            &pool,
            &[
                (vec![], vec![pks[0], pks[2]]),
                (vec![aks[1]], vec![pks[1]]),
                (vec![aks[2]], vec![]),
            ],
        );

        // Unsub program from Conn0 and Conn1
        unsub_p0().await;
        unsub_p1().await;

        // Next account -> Conn0 (first with capacity)
        let _unsub_a3 = account_subscribe(&pool, &aks[3]).await;
        // Next program -> Conn1 (Conn0 now full, Conn1 has capacity)
        let _unsub_p3 = program_subscribe(&pool, &pks[3]).await;
        // Final: Conn0 (ak3 + pk2), Conn1 (ak1 + pk3), Conn2 (ak2)
        assert_mixed_subs(
            &pool,
            &[
                (vec![aks[3]], vec![pks[2]]),
                (vec![aks[1]], vec![pks[3]]),
                (vec![aks[2]], vec![]),
            ],
        );
    }

    #[tokio::test]
    async fn test_program_elaborate_sub_unsub_lifecycle() {
        // Complex lifecycle: program subscriptions with strategic unsubs across 3 connections
        let pool = create_pool(2).await;
        let pids = create_pubkeys::<8>();

        // Sub pid0, pid1 -> Conn0 full
        let unsub0 = program_subscribe(&pool, &pids[0]).await;
        let unsub1 = program_subscribe(&pool, &pids[1]).await;
        assert_program_subs(&pool, &[vec![pids[0], pids[1]]]);

        // Sub pid2 -> Conn1 created
        let _unsub2 = program_subscribe(&pool, &pids[2]).await;
        assert_program_subs(&pool, &[vec![pids[0], pids[1]], vec![pids[2]]]);

        // Sub pid3 -> Conn1 full
        let unsub3 = program_subscribe(&pool, &pids[3]).await;
        assert_program_subs(
            &pool,
            &[vec![pids[0], pids[1]], vec![pids[2], pids[3]]],
        );

        // Sub pid4 -> Conn2 created
        let _unsub4 = program_subscribe(&pool, &pids[4]).await;
        assert_program_subs(
            &pool,
            &[
                vec![pids[0], pids[1]],
                vec![pids[2], pids[3]],
                vec![pids[4]],
            ],
        );

        // Unsub pid0 -> Conn0 has capacity
        unsub0().await;

        // Sub pid5 -> Conn0 (first-fit)
        let _unsub5 = program_subscribe(&pool, &pids[5]).await;
        assert_program_subs(
            &pool,
            &[
                vec![pids[1], pids[5]],
                vec![pids[2], pids[3]],
                vec![pids[4]],
            ],
        );

        // Unsub pid1, pid3 -> Conn0 and Conn1 each drop to 1
        unsub1().await;
        unsub3().await;

        // Sub pid6 -> Conn0 (first-fit), pid7 -> Conn1
        let _unsub6 = program_subscribe(&pool, &pids[6]).await;
        let _unsub7 = program_subscribe(&pool, &pids[7]).await;
        // Final: Conn0 (pid5, pid6), Conn1 (pid2, pid7), Conn2 (pid4)
        assert_program_subs(
            &pool,
            &[
                vec![pids[5], pids[6]],
                vec![pids[2], pids[7]],
                vec![pids[4]],
            ],
        );
    }

    #[tokio::test]
    async fn test_remove_connection_by_id_removes_target_connection() {
        let pool = create_pool(1).await;
        let [pk1, pk2, pk3] = create_pubkeys();

        let _unsub1 = account_subscribe(&pool, &pk1).await;
        let _unsub2 = account_subscribe(&pool, &pk2).await;
        let _unsub3 = account_subscribe(&pool, &pk3).await;
        assert_eq!(pool.connections.len(), 3);

        let middle_id = get_connection_at_index(&pool, 1).unwrap().id;
        let removed = pool.remove_connection_by_id(middle_id).await;
        assert!(removed.is_some());
        assert_eq!(pool.connections.len(), 2);

        let guard = Guard::new();
        let ids: Vec<u64> =
            pool.connections.iter(&guard).map(|conn| conn.id).collect();
        assert!(!ids.contains(&middle_id));
    }

    #[tokio::test]
    async fn test_subscribe_connection_error_removes_only_failing_connection() {
        let pool = PubSubConnectionPool::<FlakyByOrdinalConnection>::new(
            "mock://flaky-remove-test".to_string(),
            1,
            "test_client_flaky".to_string(),
        )
        .await
        .unwrap();
        let [pk1, pk2, pk3] = create_pubkeys();

        let (_stream, unsub1) = pool
            .account_subscribe(&pk1, RpcAccountInfoConfig::default())
            .await
            .unwrap();
        assert_eq!(pool.connections.len(), 1);

        let err = match pool
            .account_subscribe(&pk2, RpcAccountInfoConfig::default())
            .await
        {
            Ok(_) => panic!("expected connection-level subscribe error"),
            Err(err) => err,
        };
        assert!(matches!(err, PubsubClientError::ConnectionClosed(_)));
        assert_eq!(pool.connections.len(), 1);

        unsub1().await;
        let (_stream, _unsub) = pool
            .account_subscribe(&pk3, RpcAccountInfoConfig::default())
            .await
            .unwrap();

        let conn0 = get_connection_at_index(&pool, 0).unwrap();
        assert!(conn0.connection.account_subs().contains(&pk1));
        assert!(conn0.connection.account_subs().contains(&pk3));
    }

    #[tokio::test]
    async fn test_subscribe_logical_error_keeps_connection_in_pool() {
        let pool =
            PubSubConnectionPool::<LogicalErrorByOrdinalConnection>::new(
                "mock://logical-error-test".to_string(),
                1,
                "test_client_logical".to_string(),
            )
            .await
            .unwrap();
        let [pk1, pk2] = create_pubkeys();

        let _ = pool
            .account_subscribe(&pk1, RpcAccountInfoConfig::default())
            .await
            .unwrap();
        assert_eq!(pool.connections.len(), 1);

        let err = match pool
            .account_subscribe(&pk2, RpcAccountInfoConfig::default())
            .await
        {
            Ok(_) => panic!("expected logical subscribe error"),
            Err(err) => err,
        };
        assert!(matches!(err, PubsubClientError::SubscribeFailed { .. }));
        assert_eq!(pool.connections.len(), 2);
    }
}
