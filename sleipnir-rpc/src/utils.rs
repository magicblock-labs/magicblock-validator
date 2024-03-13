use jsonrpc_core::{Error, Result};
use sleipnir_bank::bank::Bank;
use sleipnir_rpc_client_api::response::{
    Response as RpcResponse, RpcResponseContext,
};
use solana_sdk::pubkey::Pubkey;

pub(crate) fn verify_pubkey(input: &str) -> Result<Pubkey> {
    input
        .parse()
        .map_err(|e| Error::invalid_params(format!("Invalid param: {e:?}")))
}

pub(crate) fn new_response<T>(bank: &Bank, value: T) -> RpcResponse<T> {
    RpcResponse {
        context: RpcResponseContext::new(bank.slot()),
        value,
    }
}
