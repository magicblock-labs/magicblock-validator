#![allow(deprecated)]
use jsonrpc_core::{BoxFuture, Result};
use jsonrpc_derive::rpc;
use solana_rpc_client_api::{
    deprecated_config::RpcGetConfirmedSignaturesForAddress2Config,
    response::RpcConfirmedTransactionStatusWithSignature,
};

#[rpc]
pub trait Deprecated {
    type Metadata;
    // RPC methods deprecated in v1.7

    // DEPRECATED
    #[rpc(meta, name = "getConfirmedSignaturesForAddress2")]
    fn get_confirmed_signatures_for_address2(
        &self,
        meta: Self::Metadata,
        address: String,
        config: Option<RpcGetConfirmedSignaturesForAddress2Config>,
    ) -> BoxFuture<Result<Vec<RpcConfirmedTransactionStatusWithSignature>>>;
}
