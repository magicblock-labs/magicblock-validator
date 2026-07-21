use ledger::request::{BlockDetails, BlockParams, BlockResponse};
use solana_rpc_client_api::config::RpcBlockConfig;
use solana_transaction_status::{
    BlockEncodingOptions, ConfirmedBlock, TransactionDetails, UiConfirmedBlock,
    UiTransactionEncoding,
};

use super::prelude::*;
use crate::engine_types::confirmed_transaction;

impl HttpDispatcher {
    /// Handles the `getBlock` RPC request.
    ///
    /// Fetches the full content of a block for a given slot number. The level of
    /// detail and transaction encoding can be customized via an optional configuration
    /// object. Returns `null` if the block is not found in the ledger.
    pub(crate) async fn get_block(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
        let (slot, config) =
            parse_params!(request.params()?, Slot, RpcBlockConfig);
        let slot = some_or_err!(slot);
        let config = config.unwrap_or_default();

        let encoding = config.encoding.unwrap_or(UiTransactionEncoding::Json);
        let transaction_details =
            config.transaction_details.unwrap_or_default();
        let options = BlockEncodingOptions {
            transaction_details,
            show_rewards: config.rewards.unwrap_or(true),
            max_supported_transaction_version: config
                .max_supported_transaction_version,
        };

        let details = match transaction_details {
            TransactionDetails::Full | TransactionDetails::Accounts => {
                BlockDetails::Full
            }
            TransactionDetails::Signatures => BlockDetails::Signatures,
            TransactionDetails::None => BlockDetails::None,
        };
        let block = self
            .engine
            .blocks()
            .get(BlockParams { slot, details })
            .await
            .map_err(RpcError::internal)?;

        let encoded_block = if let Some(block) = block {
            Some(encode_engine_block(block, encoding, options)?)
        } else {
            self.ledger
                .get_block(slot)?
                .map(ConfirmedBlock::from)
                .map(|block| {
                    block.encode_with_options(encoding, options).map_err(
                        |error| {
                            RpcError::internal(format!(
                                "failed to encode legacy block: {error}"
                            ))
                        },
                    )
                })
                .transpose()?
        };

        Ok(ResponsePayload::encode_no_context(
            &request.id,
            encoded_block,
        ))
    }
}

fn encode_engine_block(
    response: BlockResponse,
    encoding: UiTransactionEncoding,
    options: BlockEncodingOptions,
) -> Result<UiConfirmedBlock, RpcError> {
    let block = *response.block();
    let previous_blockhash = solana_hash::Hash::default().to_string();
    let blockhash = block.hash.to_string();
    let parent_slot = block.slot.saturating_sub(1);

    match response {
        BlockResponse::Full(full) => {
            let transactions = full
                .transactions
                .into_iter()
                .map(|transaction| {
                    confirmed_transaction(transaction, Some(block.time))
                        .map(|transaction| transaction.tx_with_meta)
                })
                .collect::<Result<Vec<_>, _>>()?;
            ConfirmedBlock {
                previous_blockhash,
                blockhash,
                parent_slot,
                transactions,
                rewards: Vec::new(),
                num_partitions: None,
                block_time: Some(block.time),
                block_height: Some(block.slot),
            }
            .encode_with_options(encoding, options)
            .map_err(|error| {
                RpcError::internal(format!(
                    "failed to encode engine block: {error}"
                ))
            })
        }
        BlockResponse::WithSignatures(block) => Ok(UiConfirmedBlock {
            previous_blockhash,
            blockhash,
            parent_slot,
            transactions: None,
            signatures: Some(
                block
                    .signatures
                    .into_iter()
                    .map(|s| s.to_string())
                    .collect(),
            ),
            rewards: options.show_rewards.then(Vec::new),
            num_reward_partitions: None,
            block_time: Some(block.block.time),
            block_height: Some(block.block.slot),
        }),
        BlockResponse::Bare(block) => Ok(UiConfirmedBlock {
            previous_blockhash,
            blockhash,
            parent_slot,
            transactions: None,
            signatures: None,
            rewards: options.show_rewards.then(Vec::new),
            num_reward_partitions: None,
            block_time: Some(block.time),
            block_height: Some(block.slot),
        }),
        BlockResponse::WithTransactions(_) => Err(RpcError::internal(
            "engine returned transaction-only block for an unsupported detail request",
        )),
    }
}
