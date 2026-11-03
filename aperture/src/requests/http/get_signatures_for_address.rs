use super::{HandlerResult, RpcHandlers};
use crate::requests::{
    JsonHttpRequest as JsonRequest,
    params::{Serde32Bytes, SerdeSignature},
    payload::ResponsePayload,
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

        let address = request.required::<Serde32Bytes>(0)?.into();
        let config = request.optional::<Config>(1)?.unwrap_or_default();
        let limit = config.limit.unwrap_or(DEFAULT_SIGNATURES_LIMIT).min(DEFAULT_SIGNATURES_LIMIT);
        let before = config.before.map(Into::into);
        let until = config.until.map(Into::into);

        let signatures = self.history.signatures(address, before, until, limit).await?;

        ResponsePayload::encode_no_context(&request.id, signatures)
    }
}
