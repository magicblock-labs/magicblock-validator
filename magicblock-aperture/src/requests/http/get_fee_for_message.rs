use base64::{prelude::BASE64_STANDARD, Engine};
use solana_message::{
    SanitizedMessage, SanitizedVersionedMessage, SimpleAddressLoader,
    VersionedMessage,
};

use super::prelude::*;

impl HttpDispatcher {
    /// Handles the `getFeeForMessage` RPC request.
    ///
    /// Calculates the estimated fee for a given transaction message. The calculation
    /// accounts for the number of signatures, the validator's base fee
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
        let sanitized_message = SanitizedMessage::try_new(
            sanitized_versioned_message,
            SimpleAddressLoader::Disabled,
            &Default::default(),
        )
        .map_err(RpcError::transaction_verification)?;

        let fee = signature_fee(&sanitized_message, self.context.base_fee);

        let slot = self.blocks.block_height();
        Ok(ResponsePayload::encode(&request.id, fee, slot))
    }
}

fn signature_fee(
    message: &SanitizedMessage,
    lamports_per_signature: u64,
) -> u64 {
    if lamports_per_signature == 0 {
        return 0;
    }

    message
        .get_signature_details()
        .total_signatures()
        .saturating_mul(lamports_per_signature)
}
