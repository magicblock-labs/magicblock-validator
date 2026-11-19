use ledger::request::{BlockDetails, BlockParams};
use solana_rpc_client_api::config::{RpcEncodingConfigWrapper, RpcTransactionConfig};
use solana_transaction_status::{ConfirmedTransactionWithStatusMeta, UiTransactionEncoding};

use super::{HandlerResult, RpcHandlers};
use crate::{
    error::RpcError,
    requests::{JsonHttpRequest as JsonRequest, params::SerdeSignature, payload::ResponsePayload},
    transaction::retained_transaction,
};

impl RpcHandlers {
    pub(crate) async fn get_transaction(&self, request: &JsonRequest) -> HandlerResult {
        let signature = request.required::<SerdeSignature>(0)?.into();
        let config = request
            .optional::<RpcEncodingConfigWrapper<RpcTransactionConfig>>(1)?
            .map(|config| config.convert_to_current())
            .unwrap_or_default();
        let encoding = config.encoding.unwrap_or(UiTransactionEncoding::Json);

        let Some(transaction) =
            self.engine.transactions().get(signature).await.map_err(RpcError::internal)?
        else {
            return ResponsePayload::encode_no_context(&request.id, json::Value::Null);
        };
        let slot = transaction.execution.header.slot;
        let block_time = self
            .engine
            .blocks()
            .get(BlockParams {
                slot,
                details: BlockDetails::None,
            })
            .await
            .map_err(RpcError::internal)?
            .map(|block| block.block().time);
        let transaction = ConfirmedTransactionWithStatusMeta {
            slot,
            tx_with_meta: retained_transaction(transaction)?,
            block_time,
            // Engine does not persist a transaction's block index.
            index: 0,
        };
        // Aperture can encode every supported transaction version.
        let encoded = transaction.encode(encoding, Some(u8::MAX)).ok();
        ResponsePayload::encode_no_context(&request.id, encoded)
    }
}
