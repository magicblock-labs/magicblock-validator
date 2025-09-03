use solana_rpc_client_api::config::RpcBlockConfig;
use solana_transaction_status::{
    BlockEncodingOptions, ConfirmedBlock, UiTransactionEncoding,
};

use super::prelude::*;

impl HttpDispatcher {
    /// Handles the `getBlock` RPC request.
    ///
    /// Fetches the full content of a block for a given slot number. The level of
    /// detail and transaction encoding can be customized via an optional configuration
    /// object. Returns `null` if the block is not found in the ledger.
    pub(crate) fn get_block(&self, request: &mut JsonRequest) -> HandlerResult {
        let (slot, config) =
            parse_params!(request.params()?, Slot, RpcBlockConfig);
        let slot = some_or_err!(slot);
        let config = config.unwrap_or_default();

        let encoding = config.encoding.unwrap_or(UiTransactionEncoding::Json);
        let options = BlockEncodingOptions {
            transaction_details: config.transaction_details.unwrap_or_default(),
            show_rewards: config.rewards.unwrap_or(true),
            max_supported_transaction_version: config
                .max_supported_transaction_version,
        };

        // Fetch the raw block from the ledger.
        let block = self.ledger.get_block(slot)?;

        // If the block exists, encode it for the RPC response according to the specified options.
        let encoded_block = block
            .map(ConfirmedBlock::from)
            .and_then(|b| b.encode_with_options(encoding, options).ok());

        Ok(ResponsePayload::encode_no_context(
            &request.id,
            encoded_block,
        ))
    }
}
