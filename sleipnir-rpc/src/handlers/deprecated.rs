#![allow(deprecated)]
use jsonrpc_core::{futures::future, BoxFuture, Result};
use sleipnir_rpc_client_api::{
    config::{RpcContextConfig, RpcGetConfirmedSignaturesForAddress2Config},
    response::RpcConfirmedTransactionStatusWithSignature,
};

use crate::{
    json_rpc_request_processor::JsonRpcRequestProcessor,
    traits::rpc_deprecated::Deprecated,
    utils::verify_and_parse_signatures_for_address_params,
};

pub struct DeprecatedImpl;

impl Deprecated for DeprecatedImpl {
    type Metadata = JsonRpcRequestProcessor;

    fn get_confirmed_signatures_for_address2(
        &self,
        meta: Self::Metadata,
        address: String,
        config: Option<RpcGetConfirmedSignaturesForAddress2Config>,
    ) -> BoxFuture<Result<Vec<RpcConfirmedTransactionStatusWithSignature>>>
    {
        // Exact copy of: ./full.rs get_signatures_for_address
        let config = config.unwrap_or_default();
        let commitment = config.commitment;
        let verification = verify_and_parse_signatures_for_address_params(
            address,
            config.before,
            config.until,
            config.limit,
        );

        match verification {
            Err(err) => Box::pin(future::err(err)),
            Ok((address, before, until, limit)) => Box::pin(async move {
                meta.get_signatures_for_address(
                    address,
                    before,
                    until,
                    limit,
                    RpcContextConfig {
                        commitment,
                        min_context_slot: None,
                    },
                )
                .await
            }),
        }
    }
}
