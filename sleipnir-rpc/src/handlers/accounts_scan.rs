// NOTE: from rpc/src/rpc.rs :3168
use jsonrpc_core::{Error, Result};
use log::*;
use sleipnir_rpc_client_api::{
    config::{RpcProgramAccountsConfig, RpcSupplyConfig},
    request::MAX_GET_PROGRAM_ACCOUNT_FILTERS,
    response::{
        OptionalContext, Response as RpcResponse, RpcKeyedAccount, RpcSupply,
    },
};

use crate::{
    filters::verify_filter,
    json_rpc_request_processor::JsonRpcRequestProcessor,
    traits::rpc_accounts_scan::AccountsScan, utils::verify_pubkey,
};

pub struct AccountsScanImpl;
impl AccountsScan for AccountsScanImpl {
    type Metadata = JsonRpcRequestProcessor;

    fn get_program_accounts(
        &self,
        meta: Self::Metadata,
        program_id_str: String,
        config: Option<RpcProgramAccountsConfig>,
    ) -> Result<OptionalContext<Vec<RpcKeyedAccount>>> {
        debug!(
            "get_program_accounts rpc request received: {:?}",
            program_id_str
        );
        let program_id = verify_pubkey(&program_id_str)?;
        let (config, filters, with_context) = if let Some(config) = config {
            (
                Some(config.account_config),
                config.filters.unwrap_or_default(),
                config.with_context.unwrap_or_default(),
            )
        } else {
            (None, vec![], false)
        };
        if filters.len() > MAX_GET_PROGRAM_ACCOUNT_FILTERS {
            return Err(Error::invalid_params(format!(
                    "Too many filters provided; max {MAX_GET_PROGRAM_ACCOUNT_FILTERS}"
                )));
        }
        for filter in &filters {
            verify_filter(filter)?;
        }
        meta.get_program_accounts(&program_id, config, filters, with_context)
    }

    fn get_supply(
        &self,
        meta: Self::Metadata,
        config: Option<RpcSupplyConfig>,
    ) -> Result<RpcResponse<RpcSupply>> {
        debug!("get_supply rpc request received");
        Ok(meta.get_supply(config)?)
    }
}
