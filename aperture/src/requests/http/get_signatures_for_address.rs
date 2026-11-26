use ledger::request::AccountSignaturesParams;
use solana_rpc_client_api::response::RpcConfirmedTransactionStatusWithSignature;
use solana_transaction_status::TransactionConfirmationStatus;

use super::{HandlerResult, RpcHandlers};
use crate::{
    error::RpcError,
    requests::{
        JsonHttpRequest as JsonRequest,
        params::{Serde32Bytes, SerdeSignature},
        payload::ResponsePayload,
    },
};

const DEFAULT_SIGNATURES_LIMIT: usize = 1_000;

impl RpcHandlers {
    pub(crate) async fn get_signatures_for_address(&self, request: &JsonRequest) -> HandlerResult {
        #[derive(serde::Deserialize, Default)]
        #[serde(rename_all = "camelCase")]
        struct Config {
            until: Option<SerdeSignature>,
            before: Option<SerdeSignature>,
            limit: Option<usize>,
        }

        let pubkey = request.required::<Serde32Bytes>(0)?.into();
        let config = request.optional::<Config>(1)?.unwrap_or_default();
        let limit = config.limit.unwrap_or(DEFAULT_SIGNATURES_LIMIT).min(DEFAULT_SIGNATURES_LIMIT);

        let signatures = self
            .engine
            .accounts()
            .signatures(AccountSignaturesParams {
                pubkey,
                limit,
                before: config.before.map(Into::into),
                until: config.until.map(Into::into),
            })
            .await
            .map_err(RpcError::internal)?
            .into_iter()
            .map(|info| RpcConfirmedTransactionStatusWithSignature {
                signature: info.signature.to_string(),
                slot: info.slot,
                err: info.result.err().map(Into::into),
                memo: None,
                block_time: (info.blocktime != 0).then_some(info.blocktime),
                confirmation_status: Some(TransactionConfirmationStatus::Finalized),
                transaction_index: None,
            })
            .collect::<Vec<_>>();

        ResponsePayload::encode_no_context(&request.id, signatures)
    }
}
