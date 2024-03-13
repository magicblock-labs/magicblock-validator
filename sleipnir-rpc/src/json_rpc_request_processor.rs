use std::sync::Arc;

use jsonrpc_core::Metadata;
use jsonrpc_core::Result;
use sleipnir_bank::bank::Bank;
use sleipnir_rpc_client_api::{
    config::{RpcAccountInfoConfig, UiAccount},
    response::Response as RpcResponse,
};
use solana_sdk::pubkey::Pubkey;

// NOTE: from rpc/src/rpc.rs :140
#[derive(Debug, Default, Clone)]
pub struct JsonRpcConfig {
    pub enable_rpc_transaction_history: bool,
    pub enable_extended_tx_metadata_storage: bool,
    pub health_check_slot_distance: u64,
    pub max_multiple_accounts: Option<usize>,
    pub rpc_threads: usize,
    pub rpc_niceness_adj: i8,
    pub full_api: bool,
    pub max_request_body_size: Option<usize>,
    /// Disable the health check, used for tests and TestValidator
    pub disable_health_check: bool,
}

// NOTE: from rpc/src/rpc.rs :193
#[derive(Clone)]
pub struct JsonRpcRequestProcessor {
    bank: Arc<Bank>,
    pub(crate) config: JsonRpcConfig,
}
impl Metadata for JsonRpcRequestProcessor {}

impl JsonRpcRequestProcessor {
    pub fn get_account_info(
        &self,
        pubkey: &Pubkey,
        _config: Option<RpcAccountInfoConfig>,
    ) -> Result<RpcResponse<Option<UiAccount>>> {
        let _acc = self.bank.get_account(pubkey);
        todo!("get_account_info")
    }

    pub fn get_multiple_accounts(
        &self,
        _pubkeys: Vec<Pubkey>,
        _config: Option<RpcAccountInfoConfig>,
    ) -> Result<RpcResponse<Vec<Option<UiAccount>>>> {
        todo!("get_multiple_accounts")
    }
}
