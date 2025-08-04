use solana_transaction_status_client_types::TransactionStatus;

use super::prelude::*;

impl HttpDispatcher {
    pub(crate) fn get_signature_statuses(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
        let signatures = parse_params!(request.params()?, Vec<SerdeSignature>);
        let signatures = signatures.ok_or_else(|| {
            RpcError::invalid_params("missing or invalid signatures")
        })?;
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
        Ok(ResponsePayload::encode(&request.id, statuses, slot))
    }
}
