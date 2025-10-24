use solana_rpc_client_api::config::RpcTransactionConfig;
use solana_transaction_status::UiTransactionEncoding;

use super::prelude::*;

impl HttpDispatcher {
    /// Handles the `getTransaction` RPC request.
    ///
    /// Fetches the details of a confirmed transaction from the ledger by its
    /// signature. Returns `null` if the transaction is not found.
    pub(crate) fn get_transaction(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
        let (signature, config) = parse_params!(
            request.params()?,
            SerdeSignature,
            RpcTransactionConfig
        );
        let signature = some_or_err!(signature);
        let config = config.unwrap_or_default();

        // Fetch the complete transaction details from the persistent ledger.
        let transaction =
            self.ledger.get_complete_transaction(signature, u64::MAX)?;

        let encoding = config.encoding.unwrap_or(UiTransactionEncoding::Json);
        // This implementation supports all transaction versions, so we pass a max version number.
        let max_version = Some(u8::MAX);

        // If the transaction was found, encode it for the RPC response.
        let encoded_transaction =
            transaction.and_then(|tx| tx.encode(encoding, max_version).ok());

        Ok(ResponsePayload::encode_no_context(
            &request.id,
            encoded_transaction,
        ))
    }
}
