// NOTE: from rpc/src/rpc.rs :3014
use jsonrpc_core::{Error, Result};
use log::*;
use solana_account_decoder::UiAccount;
use solana_rpc_client_api::{
    config::RpcAccountInfoConfig, request::MAX_MULTIPLE_ACCOUNTS,
    response::Response as RpcResponse,
};

use crate::{
    json_rpc_request_processor::JsonRpcRequestProcessor,
    traits::rpc_accounts::AccountsData, utils::verify_pubkey,
};

pub struct AccountsDataImpl;
impl AccountsData for AccountsDataImpl {
    type Metadata = JsonRpcRequestProcessor;

    fn get_account_info(
        &self,
        meta: Self::Metadata,
        pubkey_str: String,
        config: Option<RpcAccountInfoConfig>,
    ) -> Result<RpcResponse<Option<UiAccount>>> {
        debug!("get_account_info rpc request received: {:?}", pubkey_str);
        let pubkey = verify_pubkey(&pubkey_str)?;
        meta.get_account_info(&pubkey, config)
    }

    fn get_multiple_accounts(
        &self,
        meta: Self::Metadata,
        pubkey_strs: Vec<String>,
        config: Option<RpcAccountInfoConfig>,
    ) -> Result<RpcResponse<Vec<Option<UiAccount>>>> {
        debug!(
            "get_multiple_accounts rpc request received: {:?}",
            pubkey_strs.len()
        );

        let max_multiple_accounts = meta
            .config
            .max_multiple_accounts
            .unwrap_or(MAX_MULTIPLE_ACCOUNTS);
        if pubkey_strs.len() > max_multiple_accounts {
            return Err(Error::invalid_params(format!(
                "Too many inputs provided; max {max_multiple_accounts}"
            )));
        }
        let pubkeys = pubkey_strs
            .into_iter()
            .map(|pubkey_str| verify_pubkey(&pubkey_str))
            .collect::<Result<Vec<_>>>()?;
        meta.get_multiple_accounts(pubkeys, config)
    }
}
