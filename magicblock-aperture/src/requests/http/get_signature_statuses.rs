use solana_rpc_client_api::request::MAX_GET_SIGNATURE_STATUSES_QUERY_ITEMS;
use solana_transaction_error::TransactionError;
use solana_transaction_status::{
    TransactionConfirmationStatus, TransactionStatus,
};

use super::prelude::*;

const DEFAULT_CONFIRMATION_STATUS: Option<TransactionConfirmationStatus> =
    Some(TransactionConfirmationStatus::Finalized);

impl HttpDispatcher {
    /// Handles the `getSignatureStatuses` RPC request.
    ///
    /// Fetches the processing status for a list of transaction signatures.
    ///
    /// This handler employs a two-level lookup strategy for performance: it first
    /// checks a hot in-memory cache of recent transactions before falling back to the
    /// persistent ledger. The returned list has the same length as the input, with
    /// `null` entries for signatures that are not found.
    pub(crate) async fn get_signature_statuses(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
        let signatures = parse_params!(request.params()?, Vec<SerdeSignature>);
        let signatures: Vec<_> = some_or_err!(signatures);
        if signatures.len() > MAX_GET_SIGNATURE_STATUSES_QUERY_ITEMS {
            return Err(RpcError::invalid_params(
                "too many signatures were requested, max allowed: 256",
            ));
        }
        let mut statuses = Vec::with_capacity(signatures.len());

        for signature in signatures.into_iter().map(Into::into) {
            // Level 1: Ask the engine, which owns the recent status cache.
            if let Some(status) = self
                .engine
                .transactions()
                .status(signature)
                .await
                .map_err(RpcError::internal)?
            {
                statuses.push(Some(build_transaction_status(
                    status.slot,
                    status.result.clone(),
                )));
                continue;
            }

            // Level 2: Fall back to the deprecated ledger for historical lookups.
            let ledger_status =
                self.ledger.get_transaction_status(signature, Slot::MAX)?;
            if let Some((slot, meta)) = ledger_status {
                let status = build_transaction_status(slot, meta.status);
                statuses.push(Some(status));
            } else {
                // The signature was not found in the engine or the ledger.
                statuses.push(None);
            }
        }

        let slot = self.engine.blocks().latest().slot;
        Ok(ResponsePayload::encode(&request.id, statuses, slot))
    }
}

fn build_transaction_status(
    slot: Slot,
    status: Result<(), TransactionError>,
) -> TransactionStatus {
    TransactionStatus {
        slot,
        status: status.clone(),
        confirmations: None,
        err: status.err(),
        confirmation_status: DEFAULT_CONFIRMATION_STATUS,
    }
}
