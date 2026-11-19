use engine::Engine;
use parking_lot::Mutex;
use solana_rpc_client_api::response::RpcPerfSample;
use std::{collections::VecDeque, sync::Arc, time::Duration};

use crate::{RpcResult, account::Accounts, requests::payload::JsonBody};

/// A serialized RPC envelope, independent of HTTP status and headers.
pub(crate) type HandlerResult = RpcResult<JsonBody>;

mod blocks;
pub(crate) mod get_account_info;
pub(crate) mod get_balance;
pub(crate) mod get_delegation_status;
pub(crate) mod get_fee_for_message;
pub(crate) mod get_identity;
pub(crate) mod get_latest_blockhash;
pub(crate) mod get_multiple_accounts;
pub(crate) mod get_program_accounts;
pub(crate) mod get_recent_performance_samples;
pub(crate) mod get_signature_statuses;
pub(crate) mod get_signatures_for_address;
pub(crate) mod get_slot;
pub(crate) mod get_token_account_balance;
pub(crate) mod get_token_accounts;
pub(crate) mod get_transaction;
pub(crate) mod get_version;
pub(crate) mod is_blockhash_valid;
pub(crate) mod mocked;
pub(crate) mod request_airdrop;
pub(crate) mod send_transaction;
pub(crate) mod simulate_transaction;

/// RPC operations backed by Engine, with account materialization and service-local samples.
pub(crate) struct RpcHandlers {
    pub(crate) blocktime_ms: u64,
    pub(crate) engine: Engine,
    pub(crate) accounts: Accounts,
    /// Newest first; the collector replaces equal slots and caps retention at 720.
    pub(crate) samples: Mutex<VecDeque<RpcPerfSample>>,
}

impl RpcHandlers {
    pub(crate) fn new(engine: Engine, accounts: Accounts, blocktime: Duration) -> Arc<Self> {
        Arc::new(Self {
            blocktime_ms: blocktime.as_millis() as u64,
            accounts,
            engine,
            samples: Mutex::new(VecDeque::new()),
        })
    }
}
mod dispatch;
