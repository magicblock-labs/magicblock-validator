#![allow(dead_code)]

pub(crate) mod http;
pub(crate) mod websocket;
pub(crate) mod yellowstone;
use base64::{Engine, engine::general_purpose::STANDARD};
use delegation_program_api::{
    pda::delegation_record_pda_from_delegated_account, state::DelegationRecord,
};
use serde_json::{Value, json};
use solana_account::{AccountBuilder, StateFlags};
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;
use std::sync::Arc;
use std::{future::Future, time::Duration};
use tokio::time;
use tokio_util::{sync::CancellationToken, task::TaskTracker};
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

use yellowstone_grpc_geyser::plugin::message::{Message, MessageAccount, MessageAccountInfo};
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

/// One provider-independent account image for snapshots, notifications, and Geyser messages.
#[derive(Clone)]
pub(crate) struct Account {
    pub(crate) key: Pubkey,
    pub(crate) owner: Pubkey,
    pub(crate) lamports: u64,
    pub(crate) data: Vec<u8>,
    pub(crate) executable: bool,
}

impl Account {
    /// Non-executable, rent-funded image with one byte of recognizable data.
    pub(crate) fn new(key: Pubkey) -> Self {
        Self {
            key,
            owner: Pubkey::default(),
            lamports: 1_000_000,
            data: ACCOUNT_DATA.to_vec(),
            executable: false,
        }
    }

    /// Shares an Engine testkit image across the HTTP, WebSocket, and plugin encoders.
    pub(crate) fn from_builder(key: Pubkey, image: &AccountBuilder) -> Self {
        let image = image.read();
        Self {
            key,
            owner: image.owner(),
            lamports: image.lamports(),
            data: image.data().to_vec(),
            executable: image.flags().contains(StateFlags::EXECUTABLE),
        }
    }

    /// Confirmed RPC account encoding shared by HTTP snapshots and WebSocket notifications.
    pub(crate) fn rpc(&self) -> Value {
        let compressed = zstd::stream::encode_all(self.data.as_slice(), 0).unwrap();
        json!({"owner": self.owner.to_string(), "lamports": self.lamports,
            "executable": self.executable, "data": [STANDARD.encode(compressed), "base64+zstd"]})
    }

    /// Plugin input image with a transaction signature, eligible for DLP account filters.
    pub(crate) fn geyser(&self, slot: u64) -> Message {
        Message::Account(Arc::new(MessageAccount {
            account: MessageAccountInfo {
                pubkey: self.key,
                owner: self.owner,
                lamports: self.lamports,
                data: self.data.clone().into(),
                executable: self.executable,
                rent_epoch: 0,
                write_version: slot,
                txn_signature: Some(Default::default()),
                pre_encoded: Default::default(),
            },
            slot,
            is_startup: false,
            created_at: Default::default(),
        }))
    }
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

/// Application and canonical record images shared by all provider encoders.
pub(crate) fn delegation(
    key: Pubkey,
    owner: Pubkey,
    authority: Pubkey,
    slot: u64,
    actions: &[Instruction],
) -> (Account, Account) {
    use delegation_program_api::compact::ClearText;
    let mut account = Account::new(key);
    account.owner = delegation_program_api::id();
    account.data = V42_INITIAL.to_le_bytes().to_vec();
    let metadata = DelegationRecord {
        owner,
        authority,
        delegation_slot: slot,
        lamports: account.lamports,
        commit_frequency_ms: 1,
    };
    let mut record = Account::new(delegation_record_pda_from_delegated_account(&key));
    record.owner = delegation_program_api::id();
    record.data = encode_record(&metadata);
    if !actions.is_empty() {
        record.data.extend(borsh::to_vec(&actions.to_vec().cleartext()).unwrap());
    }
    (account, record)
}

/// Canonical record header shared by ordinary and eATA delegation fixtures.
fn encode_record(metadata: &DelegationRecord) -> Vec<u8> {
    let mut data = vec![0; DelegationRecord::size_with_discriminator()];
    metadata.to_bytes_with_discriminator(&mut data).unwrap();
    data
}
