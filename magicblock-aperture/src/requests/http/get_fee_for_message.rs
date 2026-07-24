use base64::{prelude::BASE64_STANDARD, Engine};
use solana_message::{
    SanitizedMessage, SanitizedVersionedMessage, SimpleAddressLoader,
    VersionedMessage,
};

use super::prelude::*;

impl HttpDispatcher {
    /// Handles the `getFeeForMessage` RPC request.
    ///
    /// Validates the message and returns the validator's zero execution fee.
    pub(crate) fn get_fee_for_message(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
        let message_b64 = parse_params!(request.params()?, String);
        let message_b64: String = some_or_err!(message_b64);

        // Decode and deserialize the transaction message.
        let message_bytes = BASE64_STANDARD
            .decode(message_b64)
            .map_err(RpcError::parse_error)?;
        let versioned_message: VersionedMessage =
            bincode::deserialize(&message_bytes)
                .map_err(RpcError::invalid_params)?;

        // Sanitize the message for processing.
        let sanitized_versioned_message =
            SanitizedVersionedMessage::try_new(versioned_message)
                .map_err(RpcError::transaction_verification)?;
        SanitizedMessage::try_new(
            sanitized_versioned_message,
            SimpleAddressLoader::Disabled,
            &Default::default(),
        )
        .map_err(RpcError::transaction_verification)?;

        let slot = self.blocks.block_height();
        Ok(ResponsePayload::encode(&request.id, 0, slot))
    }
}
