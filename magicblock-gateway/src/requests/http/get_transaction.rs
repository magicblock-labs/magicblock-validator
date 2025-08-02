use hyper::Response;
use solana_rpc_client_api::config::RpcTransactionConfig;
use solana_transaction_status_client_types::UiTransactionEncoding;

use crate::{
    error::RpcError,
    requests::{params::SerdeSignature, payload::ResponsePayload, JsonRequest},
    server::http::dispatch::HttpDispatcher,
    unwrap,
    utils::JsonBody,
};

impl HttpDispatcher {
    pub(crate) fn get_transaction(
        &self,
        request: JsonRequest,
    ) -> Response<JsonBody> {
        let params = request
            .params
            .ok_or_else(|| RpcError::invalid_request("missing params"));
        unwrap!(mut params, request.id);
        let (signature, config) =
            parse_params!(params, SerdeSignature, RpcTransactionConfig);
        let signature = signature.ok_or_else(|| {
            RpcError::invalid_params("missing or invalid signature")
        });
        unwrap!(signature, request.id);
        let config = config.unwrap_or_default();
        let transaction = self
            .ledger
            .get_complete_transaction(signature.0, u64::MAX)
            .map_err(RpcError::internal);
        unwrap!(transaction, request.id);

        let encoding = config.encoding.unwrap_or(UiTransactionEncoding::Json);
        let txn = transaction.and_then(|tx| tx.encode(encoding, None).ok());
        Response::new(ResponsePayload::encode_no_context(&request.id, txn))
    }
}
