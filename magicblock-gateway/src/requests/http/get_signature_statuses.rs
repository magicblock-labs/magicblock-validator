use hyper::Response;
use solana_transaction_status_client_types::TransactionStatus;

use crate::{
    error::RpcError,
    requests::{params::SerdeSignature, payload::ResponsePayload, JsonRequest},
    server::http::dispatch::HttpDispatcher,
    unwrap,
    utils::JsonBody,
    Slot,
};

impl HttpDispatcher {
    pub(crate) fn get_signature_statuses(
        &self,
        request: JsonRequest,
    ) -> Response<JsonBody> {
        let params = request
            .params
            .ok_or_else(|| RpcError::invalid_request("missing params"));
        unwrap!(mut params, request.id);
        let signatures = parse_params!(params, Vec<SerdeSignature>);
        let signatures = signatures.ok_or_else(|| {
            RpcError::invalid_params("missing or invalid signatures")
        });
        unwrap!(signatures, request.id);
        let mut statuses = Vec::with_capacity(signatures.len());
        for signature in signatures {
            if let Some(status) = self.transactions.get(&signature.0) {
                if status.successful {
                    statuses.push(Some(TransactionStatus {
                        slot: status.slot,
                        status: Ok(()),
                        confirmations: None,
                        err: None,
                        confirmation_status: None,
                    }));
                    continue;
                }
            }
            let Some((slot, meta)) = self
                .ledger
                .get_transaction_status(signature.0, Slot::MAX)
                .ok()
                .flatten()
            else {
                statuses.push(None);
                continue;
            };
            statuses.push(Some(TransactionStatus {
                slot,
                status: meta.status,
                confirmations: None,
                err: None,
                confirmation_status: None,
            }));
        }
        let slot = self.accountsdb.slot();
        ResponsePayload::encode(&request.id, statuses, slot)
    }
}
