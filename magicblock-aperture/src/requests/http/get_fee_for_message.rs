use base64::{Engine, prelude::BASE64_STANDARD};
use solana_message::{
    SanitizedMessage, SanitizedVersionedMessage, SimpleAddressLoader,
    VersionedMessage,
};

use super::HandlerResult;
use crate::{
    error::RpcError,
    requests::{JsonHttpRequest as JsonRequest, payload::ResponsePayload},
    server::http::dispatch::HttpDispatcher,
};

impl HttpDispatcher {
    pub(crate) fn get_fee_for_message(
        &self,
        request: &JsonRequest,
    ) -> HandlerResult {
        let message_b64 = request.required::<String>(0)?;

        let message_bytes = BASE64_STANDARD
            .decode(message_b64)
            .map_err(RpcError::parse_error)?;
        let versioned_message: VersionedMessage =
            wincode::deserialize(&message_bytes)
                .map_err(RpcError::invalid_params)?;

        let sanitized_versioned_message =
            SanitizedVersionedMessage::try_new(versioned_message)
                .map_err(RpcError::transaction_verification)?;
        SanitizedMessage::try_new(
            sanitized_versioned_message,
            SimpleAddressLoader::Disabled,
            &Default::default(),
        )
        .map_err(RpcError::transaction_verification)?;

        let slot = self.engine.blocks().latest().slot;
        Ok(ResponsePayload::encode(&request.id, 0_u64, slot))
    }
}
