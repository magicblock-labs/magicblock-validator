use json::Deserialize;
use solana_rpc_client_api::response::RpcConfirmedTransactionStatusWithSignature;
use solana_transaction_status_client_types::TransactionConfirmationStatus;

use super::prelude::*;

const DEFAULT_SIGNATURES_LIMIT: usize = 1_000;

impl HttpDispatcher {
    pub(crate) fn get_signatures_for_address(
        &self,
        request: JsonRequest,
    ) -> Response<JsonBody> {
        let params = request
            .params
            .ok_or_else(|| RpcError::invalid_request("missing params"));
        unwrap!(mut params, request.id);
        let (address, config) = parse_params!(params, Serde32Bytes, Config);
        let address = address.map(Into::into).ok_or_else(|| {
            RpcError::invalid_params("missing or invalid address")
        });
        unwrap!(address, request.id);
        let config = config.unwrap_or_default();
        let signatures = self
            .ledger
            .get_confirmed_signatures_for_address(
                address,
                Slot::MAX,
                config.before.map(|s| s.0),
                config.until.map(|s| s.0),
                config.limit.unwrap_or(DEFAULT_SIGNATURES_LIMIT),
            )
            .map_err(RpcError::internal);
        unwrap!(signatures, request.id);
        let signatures = signatures
            .infos
            .into_iter()
            .map(|x| {
                let mut item: RpcConfirmedTransactionStatusWithSignature =
                    x.into();
                item.confirmation_status =
                    Some(TransactionConfirmationStatus::Finalized);
                item
            })
            .collect::<Vec<_>>();
        Response::new(ResponsePayload::encode_no_context(
            &request.id,
            signatures,
        ))
    }
}

#[derive(Deserialize, Default)]
struct Config {
    until: Option<SerdeSignature>,
    before: Option<SerdeSignature>,
    limit: Option<usize>,
}
