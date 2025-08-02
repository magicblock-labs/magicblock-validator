use hyper::Response;
use log::warn;
use magicblock_gateway_types::transactions::ProcessableTransaction;
use solana_message::SimpleAddressLoader;
use solana_rpc_client_api::config::RpcSendTransactionConfig;
use solana_transaction::{
    sanitized::SanitizedTransaction,
    versioned::sanitized::SanitizedVersionedTransaction,
};
use solana_transaction_status::UiTransactionEncoding;

use crate::{
    error::RpcError,
    requests::{payload::ResponsePayload, JsonRequest},
    server::http::dispatch::HttpDispatcher,
    unwrap,
    utils::JsonBody,
};

impl HttpDispatcher {
    pub(crate) async fn send_transaction(
        &self,
        request: JsonRequest,
    ) -> Response<JsonBody> {
        let params = request
            .params
            .ok_or_else(|| RpcError::invalid_request("missing params"));
        unwrap!(mut params, request.id);
        let (transaction, config) =
            parse_params!(params, String, RpcSendTransactionConfig);
        let transaction = transaction.ok_or_else(|| {
            RpcError::invalid_params("missing encoded transaction")
        });
        let config = config.unwrap_or_default();
        let encoding = config.encoding.unwrap_or(UiTransactionEncoding::Base58);
        unwrap!(transaction, request.id);
        let transaction = self.decode_transaction(&transaction, encoding);
        unwrap!(transaction, request.id);
        let signature = transaction.signatures[0];
        let hash = transaction.message.hash();
        let transaction = SanitizedVersionedTransaction::try_new(transaction)
            .map_err(RpcError::invalid_params);
        unwrap!(transaction, request.id);
        let transaction = SanitizedTransaction::try_new(
            transaction,
            hash,
            false,
            SimpleAddressLoader::Disabled,
            &Default::default(),
        )
        .map_err(RpcError::invalid_params);
        unwrap!(transaction, request.id);
        let to_execute = ProcessableTransaction {
            transaction,
            simulation: None,
            result_tx: None,
        };
        if self
            .transaction_execution_tx
            .send(to_execute)
            .await
            .is_err()
        {
            warn!("transaction execution channel has closed");
        };
        let response =
            ResponsePayload::encode_no_context(&request.id, signature);
        Response::new(response)
    }
}
