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
    ///
    /// Only the ledger fallbacks run under the blocking-read gate: cache hits
    /// (clients polling recently submitted transactions) must stay responsive
    /// even when degraded ledger reads hold every permit.
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
        let mut misses = Vec::new();

        for (index, signature) in
            signatures.into_iter().map(Into::into).enumerate()
        {
            // Level 1: Check the hot in-memory cache first.
            if let Some(Some(cached_status)) = self.transactions.get(&signature)
            {
                statuses.push(Some(build_transaction_status(
                    cached_status.slot,
                    cached_status.result.clone(),
                )));
            } else {
                statuses.push(None);
                misses.push((index, signature));
            }
        }

        // Level 2: Fall back to the persistent ledger for historical lookups.
        if !misses.is_empty() {
            let resolved = self
                .run_blocking(|| {
                    misses
                        .into_iter()
                        .map(|(index, signature)| {
                            self.ledger
                                .get_transaction_status(signature, Slot::MAX)
                                .map(|status| (index, status))
                        })
                        .collect::<Result<Vec<_>, _>>()
                })
                .await?;
            for (index, ledger_status) in resolved {
                if let Some((slot, meta)) = ledger_status {
                    statuses[index] =
                        Some(build_transaction_status(slot, meta.status));
                }
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
