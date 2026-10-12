#![allow(dead_code)]

mod account;
pub(crate) mod http;
pub(crate) mod magic;
pub(crate) mod websocket;
pub(crate) mod yellowstone;

use std::{future::Future, sync::Arc, time::Duration};

use engine::testkit::TestEngine;
use serde_json::{Value, json};
use tokio::time;
use tokio_util::{sync::CancellationToken, task::TaskTracker};
use url::Url;

// Each test target uses a different subset of these shared fixtures.
#[allow(unused_imports)]
pub(crate) use account::{Account, Ata, delegation, encode_record, undelegation_request};

use super::chainsync::{
    ChainSync, ChainSyncAccount, ChainSyncConfig, Error, GrpcStreamConfig, Result, WebSocketConfig,
    WebSocketError, WebSocketProvider,
};

/// Owns fixture tasks and cancels their entire I/O flow, including held replies.
/// Cleanup joins all tasks, so none outlive the scenario or its plugin guard.
#[derive(Clone, Default)]
struct Tasks {
    cancel: CancellationToken,
    tracker: TaskTracker,
}

impl Tasks {
    fn spawn(&self, future: impl Future<Output = ()> + Send + 'static) {
        let cancel = self.cancel.clone();
        self.tracker.spawn(async move {
            tokio::select! {
                _ = cancel.cancelled() => {},
                () = future => {},
            }
        });
    }

    async fn close(self) {
        self.cancel.cancel();
        self.tracker.close();
        within(self.tracker.wait()).await;
    }
}

/// Successful JSON-RPC envelope shared by the scripted HTTP and WebSocket providers.
fn rpc_result(id: &Value, result: impl Into<Value>) -> Value {
    json!({"jsonrpc": RPC_VERSION, "id": id, "result": result.into()})
}

/// Bounds a scenario's event-driven handshakes, including cleanup.
pub(crate) const DEADLINE: Duration = Duration::from_secs(30);

pub(crate) const LOOPBACK: &str = "127.0.0.1:0";
/// Policy-only tests construct clients without starting a transport.
pub(crate) const UNAVAILABLE: &str = "http://127.0.0.1:1";
pub(crate) const RPC_VERSION: &str = "2.0";
pub(crate) const SERVICE_ERROR: &str = "unavailable";
/// Short real-time rebuild interval for plugin-backed coverage scenarios.
pub(crate) const DUPLICATION_DELAY: Duration = Duration::from_millis(5);
/// Initial plugin history and opening checkpoint ensure the two-slot replay lookback is available.
pub(crate) const REPLAY_SLOT: u64 = 1;
pub(crate) const START_SLOT: u64 = REPLAY_SLOT + 2;
pub(crate) const READY_SLOT: u64 = START_SLOT + 1;
pub(crate) const CONTEXT_SLOT: u64 = READY_SLOT + 1;
pub(crate) const DELEGATION_SLOT: u64 = 10;
pub(crate) const PROJECTION_SLOT: u64 = 15;
pub(crate) const RPC_BATCH_LIMIT: usize = 100;
pub(crate) const FIRST_SUBSCRIPTION: u64 = 1;
pub(crate) const ACCOUNT_DATA: &[u8] = &[1];
pub(crate) const V42_INITIAL: i64 = 1;
pub(crate) const ACTION_AMOUNT: i64 = 42;
pub(crate) const TOKEN_BALANCE: u64 = 9;
pub(crate) const DELEGATED_TOKEN_BALANCE: u64 = 42;
pub(crate) const TOKEN_LAMPORTS: u64 = 1000;
pub(crate) const NATIVE_RESERVE: u64 = 500;

/// Ordinary ChainSync configuration for controlled endpoints; scenarios set replay, tracking delay, and AML.
pub(crate) fn config(http: &Url, ws: Option<&websocket::Server>, grpc: &[&Url]) -> ChainSyncConfig {
    ChainSyncConfig {
        aml: None,
        http: vec![http.clone()],
        websocket: WebSocketConfig(
            ws.into_iter()
                .map(|server| WebSocketProvider {
                    url: server.endpoint.clone(),
                    max_connections: 1,
                    subs_per_connection: 128,
                })
                .collect(),
        ),
        grpc: grpc
            .iter()
            .map(|endpoint| GrpcStreamConfig {
                endpoint: (*endpoint).clone(),
                token: None,
                duplication_delay: Duration::from_secs(30 * 60),
            })
            .collect(),
    }
}

/// Constructs ChainSync for direct handler tests; only the optional HTTP provider runs.
pub(crate) fn start_handler_sync(
    engine: &mut TestEngine,
    http: Option<&http::Server>,
) -> Arc<ChainSync> {
    let unavailable = UNAVAILABLE.parse().unwrap();
    let endpoint = http.map_or(&unavailable, |server| &server.endpoint);
    let config = config(endpoint, None, &[&unavailable]);
    start(engine, config)
}

/// Starts the real service with the scenario's configuration and Engine shutdown owner.
pub(crate) fn start(engine: &mut TestEngine, config: ChainSyncConfig) -> Arc<ChainSync> {
    ChainSync::new(engine.clone(), config, engine.shutdown()).unwrap()
}

/// Fails a stalled handshake instead of relying on sleeps or yield counts.
pub(crate) async fn within<T>(future: impl Future<Output = T>) -> T {
    time::timeout(DEADLINE, future)
        .await
        .expect("scenario exceeded handshake deadline")
}

/// Runs both sides of a scripted exchange to completion under one deadline, even when the operation fails.
pub(crate) async fn scripted<T>(
    operation: impl Future<Output = T>,
    script: impl Future<Output = ()>,
) -> T {
    within(async { tokio::join!(operation, script).0 }).await
}

/// Opens from the fixture's replayable slot one, with HTTP freshness starting at slot three.
pub(crate) fn start_chain_sync(
    engine: &mut TestEngine,
    http: &http::Server,
    ws: Option<&websocket::Server>,
    grpc: &yellowstone::Server,
) -> Arc<ChainSync> {
    engine.accounts().advance_chain_slot(START_SLOT);
    let config = config(&http.endpoint, ws, &[&grpc.endpoint]);
    start(engine, config)
}

/// Initial connection admission is retried through the public API, not timing-based readiness guesses.
pub(crate) async fn acquire(chain_sync: &ChainSync, accounts: &[ChainSyncAccount]) -> Result<()> {
    loop {
        match chain_sync.ensure(accounts).await {
            Err(Error::Subscribe(WebSocketError::Unavailable)) => continue,
            result => return result.map(|_| ()),
        }
    }
}
