use solana_rpc_client_api::config::RpcTransactionConfig;
use solana_transaction_status_client_types::UiTransactionEncoding;

use super::prelude::*;

impl HttpDispatcher {
    pub(crate) fn get_transaction(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
        let (signature, config) = parse_params!(
            request.params()?,
            SerdeSignature,
            RpcTransactionConfig
        );
        let signature: SerdeSignature = some_or_err!(signature);
        let config = config.unwrap_or_default();
        let transaction = self
            .ledger
            .get_complete_transaction(signature.0, u64::MAX)?;

        let encoding = config.encoding.unwrap_or(UiTransactionEncoding::Json);
        let txn = transaction.and_then(|tx| tx.encode(encoding, None).ok());
        Ok(ResponsePayload::encode_no_context(&request.id, txn))
    }
}
