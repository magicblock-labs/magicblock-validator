use magicblock_core::Slot;
use solana_rpc_client_api::request::MAX_GET_SIGNATURE_STATUSES_QUERY_ITEMS;
use solana_transaction_error::TransactionError;
use solana_transaction_status::{
    TransactionConfirmationStatus, TransactionStatus,
};

use super::HandlerResult;
use crate::{
    error::RpcError,
    requests::{
        JsonHttpRequest as JsonRequest, params::SerdeSignature,
        payload::ResponsePayload,
    },
    server::http::dispatch::HttpDispatcher,
};

const DEFAULT_CONFIRMATION_STATUS: Option<TransactionConfirmationStatus> =
    Some(TransactionConfirmationStatus::Finalized);

impl HttpDispatcher {
    pub(crate) async fn get_signature_statuses(
        &self,
        request: &JsonRequest,
    ) -> HandlerResult {
        let signatures = request.required::<Vec<SerdeSignature>>(0)?;
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
