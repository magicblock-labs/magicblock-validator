use base64::{prelude::BASE64_STANDARD, Engine};
use solana_compute_budget_instruction::instructions_processor::process_compute_budget_instructions;
use solana_fee_structure::FeeBudgetLimits;
use solana_message::{
    SanitizedMessage, SanitizedVersionedMessage, SimpleAddressLoader,
    VersionedMessage,
};

use super::prelude::*;

impl HttpDispatcher {
    pub(crate) fn get_fee_for_message(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
        let message = parse_params!(request.params()?, String);
        let message: String = some_or_err!(message);
        let message = BASE64_STANDARD
            .decode(message)
            .map_err(RpcError::parse_error)?;
        let message: VersionedMessage =
            bincode::deserialize(&message).map_err(RpcError::invalid_params)?;
        let message = SanitizedVersionedMessage::try_new(message)
            .map_err(RpcError::transaction_verification)?;
        let message = SanitizedMessage::try_new(
            message,
            SimpleAddressLoader::Disabled,
            &Default::default(),
        )
        .map_err(RpcError::transaction_verification)?;
        let budget = process_compute_budget_instructions(
            message
                .program_instructions_iter()
                .map(|(k, i)| (k, i.into())),
            &self.context.featureset,
        )
        .map(FeeBudgetLimits::from)?;

        let fee = solana_fee::calculate_fee(
            &message,
            self.context.base_fee == 0,
            self.context.base_fee,
            budget.prioritization_fee,
            self.context.featureset.as_ref().into(),
        );

        let slot = self.blocks.block_height();
        Ok(ResponsePayload::encode(&request.id, fee, slot))
    }
}
