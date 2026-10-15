use solana_rpc_client_api::config::{RpcEncodingConfigWrapper, RpcTransactionConfig};
use solana_transaction_status::UiTransactionEncoding;

use super::{HandlerResult, RpcHandlers};
use crate::requests::{
    JsonHttpRequest as JsonRequest, params::SerdeSignature, payload::ResponsePayload,
};

impl RpcHandlers {
    pub(crate) async fn get_transaction(&self, request: &JsonRequest) -> HandlerResult {
        let signature = request.required::<SerdeSignature>(0)?.into();
        let config = request
            .optional::<RpcEncodingConfigWrapper<RpcTransactionConfig>>(1)?
            .map(|config| config.convert_to_current())
            .unwrap_or_default();
        let encoding = config.encoding.unwrap_or(UiTransactionEncoding::Json);

        self.history
            .transaction(signature, |transaction| {
                // Aperture can encode every supported transaction version.
                let encoded = transaction.and_then(|tx| tx.encode(encoding, Some(u8::MAX)).ok());
                ResponsePayload::encode_no_context(&request.id, encoded)
            })
            .await
    }
}
