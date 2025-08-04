use json::Deserialize;
use solana_rpc_client_api::response::RpcConfirmedTransactionStatusWithSignature;
use solana_transaction_status_client_types::TransactionConfirmationStatus;

use super::prelude::*;

const DEFAULT_SIGNATURES_LIMIT: usize = 1_000;

impl HttpDispatcher {
    pub(crate) fn get_signatures_for_address(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
        let (address, config) =
            parse_params!(request.params()?, Serde32Bytes, Config);
        let address = some_or_err!(address);
        let config = config.unwrap_or_default();
        let signatures = self.ledger.get_confirmed_signatures_for_address(
            address,
            Slot::MAX,
            config.before.map(|s| s.0),
            config.until.map(|s| s.0),
            config.limit.unwrap_or(DEFAULT_SIGNATURES_LIMIT),
        )?;
        let signatures = signatures
            .infos
            .into_iter()
            .map(|x| {
                let mut i = RpcConfirmedTransactionStatusWithSignature::from(x);
                i.confirmation_status
                    .replace(TransactionConfirmationStatus::Finalized);
                i
            })
            .collect::<Vec<_>>();
        Ok(ResponsePayload::encode_no_context(&request.id, signatures))
    }
}

#[derive(Deserialize, Default)]
struct Config {
    until: Option<SerdeSignature>,
    before: Option<SerdeSignature>,
    limit: Option<usize>,
}
