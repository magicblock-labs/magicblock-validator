use solana_rpc_client_api::config::RpcBlockConfig;
use solana_transaction_status::{BlockEncodingOptions, ConfirmedBlock};
use solana_transaction_status_client_types::UiTransactionEncoding;

use super::prelude::*;

impl HttpDispatcher {
    pub(crate) fn get_block(&self, request: &mut JsonRequest) -> HandlerResult {
        let (slot, config) =
            parse_params!(request.params()?, Slot, RpcBlockConfig);
        let slot = slot.ok_or_else(|| {
            RpcError::invalid_params("missing or invalid slot")
        })?;
        let config = config.unwrap_or_default();

        let encoding = config.encoding.unwrap_or(UiTransactionEncoding::Json);
        let options = BlockEncodingOptions {
            transaction_details: config.transaction_details.unwrap_or_default(),
            show_rewards: config.rewards.unwrap_or(true),
            max_supported_transaction_version: config
                .max_supported_transaction_version,
        };
        let block = self.ledger.get_block(slot).map_err(RpcError::internal)?;
        let block = block
            .map(ConfirmedBlock::from)
            .and_then(|b| b.encode_with_options(encoding, options).ok());
        Ok(ResponsePayload::encode_no_context(&request.id, block))
    }
}
