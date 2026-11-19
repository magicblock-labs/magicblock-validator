use solana_rpc_client_api::request::MAX_GET_SIGNATURE_STATUSES_QUERY_ITEMS;
use solana_transaction_status::{TransactionConfirmationStatus, TransactionStatus};

use super::{HandlerResult, RpcHandlers};
use crate::{
    error::RpcError,
    requests::{JsonHttpRequest as JsonRequest, params::SerdeSignature, payload::ResponsePayload},
};

impl RpcHandlers {
    pub(crate) async fn get_signature_statuses(&self, request: &JsonRequest) -> HandlerResult {
        let signatures = request.required::<Vec<SerdeSignature>>(0)?;
        if signatures.len() > MAX_GET_SIGNATURE_STATUSES_QUERY_ITEMS {
            return Err(RpcError::invalid_params(
                "too many signatures were requested, max allowed: 256",
            ));
        }
        let transactions = self.engine.transactions();
        let mut statuses = Vec::with_capacity(signatures.len());
        for signature in signatures {
            let status = transactions.status(signature.into()).await.map_err(RpcError::internal)?;
            statuses.push(status.map(|status| TransactionStatus {
                slot: status.slot,
                status: status.result.clone(),
                confirmations: None,
                err: status.result.err(),
                confirmation_status: Some(TransactionConfirmationStatus::Finalized),
            }));
        }

        let slot = self.engine.blocks().latest().slot;
        ResponsePayload::encode(&request.id, statuses, slot)
    }
}
