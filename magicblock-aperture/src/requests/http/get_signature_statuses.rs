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
    pub(crate) fn get_signature_statuses(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
        let signatures = parse_params!(request.params()?, Vec<SerdeSignature>);
        let signatures: Vec<_> = some_or_err!(signatures);
        let mut statuses = Vec::with_capacity(signatures.len());

        for signature in signatures.into_iter().map(Into::into) {
            // Level 1: Check the hot in-memory cache first.
            if let Some(Some(cached_status)) = self.transactions.get(&signature)
            {
                statuses.push(Some(build_transaction_status(
                    cached_status.slot,
                    cached_status.result.clone(),
                )));
                continue;
            }

            // Level 2: Fall back to the persistent ledger for historical lookups.
            let ledger_status =
                self.ledger.get_transaction_status(signature, Slot::MAX)?;
            if let Some((slot, meta)) = ledger_status {
                let status = build_transaction_status(slot, meta.status);
                statuses.push(Some(status));
            } else {
                // The signature was not found in the cache or the ledger.
                statuses.push(None);
            }
        }

        let slot = self.blocks.block_height();
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
