use std::ops::RangeInclusive;

use ledger::request::{BlockDetails, BlockParams, BlockResponse};
use mbv_core::Slot;
use serde::{Serialize, Serializer};
use solana_rpc_client_api::config::{RpcBlockConfig, RpcBlocksConfigWrapper};
use solana_transaction_status::{
    BlockEncodingOptions, ConfirmedBlock, TransactionDetails, UiConfirmedBlock,
    UiTransactionEncoding,
};

use super::{HandlerResult, RpcHandlers};
use crate::{
    RpcResult,
    error::{BLOCK_CLEANED_UP, BLOCK_NOT_FOUND, RpcError},
    requests::{JsonHttpRequest as JsonRequest, payload::ResponsePayload},
    transaction::retained_transaction,
};

const MAX_BLOCKS: Slot = 500_000;

#[derive(serde::Deserialize)]
#[serde(untagged)]
enum BlockConfigParam {
    Config(RpcBlockConfig),
    Encoding(UiTransactionEncoding),
}

impl RpcHandlers {
    pub(crate) async fn get_block(&self, request: &JsonRequest) -> HandlerResult {
        let slot = request.required::<Slot>(0)?;
        let config = match request.optional::<BlockConfigParam>(1)? {
            Some(BlockConfigParam::Config(config)) => config,
            Some(BlockConfigParam::Encoding(encoding)) => RpcBlockConfig {
                encoding: Some(encoding),
                ..RpcBlockConfig::default()
            },
            None => RpcBlockConfig::default(),
        };

        let encoding = config.encoding.unwrap_or(UiTransactionEncoding::Json);
        let transaction_details = config.transaction_details.unwrap_or_default();
        // Match getTransaction: Aperture can encode every supported version
        // when clients omit the optional version capability.
        let options = BlockEncodingOptions {
            transaction_details,
            show_rewards: config.rewards.unwrap_or(true),
            max_supported_transaction_version: config
                .max_supported_transaction_version
                .or(Some(u8::MAX)),
        };
        let details = match transaction_details {
            TransactionDetails::Full | TransactionDetails::Accounts => BlockDetails::Full,
            TransactionDetails::Signatures => BlockDetails::Signatures,
            TransactionDetails::None => BlockDetails::None,
        };
        let encoded = self
            .read_block(BlockParams { slot, details })
            .await?
            .map(|block| encode_block(block, encoding, options))
            .transpose()?;
        ResponsePayload::encode_no_context(&request.id, encoded)
    }

    pub(crate) async fn get_block_time(&self, request: &JsonRequest) -> HandlerResult {
        let slot = request.required::<Slot>(0)?;
        let block = self
            .read_block(BlockParams {
                slot,
                details: BlockDetails::None,
            })
            .await?
            .ok_or_else(|| {
                RpcError::custom(
                    format!("Slot {slot} was skipped, or is not yet available"),
                    BLOCK_NOT_FOUND,
                )
            })?;
        ResponsePayload::encode_no_context(&request.id, block.block().time)
    }

    pub(crate) fn first_available_block(&self, request: &JsonRequest) -> HandlerResult {
        ResponsePayload::encode_no_context(&request.id, self.first_retained_slot())
    }

    pub(crate) fn get_blocks(&self, request: &JsonRequest) -> HandlerResult {
        let start = request.required::<Slot>(0)?;
        // Like Agave, the second parameter can be an end slot or a context config.
        let end = request
            .optional::<RpcBlocksConfigWrapper>(1)?
            .and_then(|wrapper| wrapper.unzip().0)
            .unwrap_or(Slot::MAX)
            .min(start.saturating_add(MAX_BLOCKS));
        self.encode_slots(request, start, end, MAX_BLOCKS + 1)
    }

    pub(crate) fn get_blocks_with_limit(&self, request: &JsonRequest) -> HandlerResult {
        let start = request.required::<Slot>(0)?;
        let limit = request.required::<Slot>(1)?.min(MAX_BLOCKS);
        self.encode_slots(request, start, Slot::MAX, limit)
    }

    fn first_retained_slot(&self) -> Slot {
        self.engine.blocks().retained_range().map_or(0, |range| *range.start())
    }

    async fn read_block(&self, params: BlockParams) -> RpcResult<Option<BlockResponse>> {
        let slot = params.slot;
        let block = self.engine.blocks().get(params).await.map_err(RpcError::internal)?;
        if block.is_some() {
            return Ok(block);
        }
        let first = self.first_retained_slot();
        if slot < first {
            return Err(RpcError::custom(
                format!(
                    "Block {slot} cleaned up, does not exist on node. First available block: {first}"
                ),
                BLOCK_CLEANED_UP,
            ));
        }
        Ok(None)
    }

    fn encode_slots(
        &self,
        request: &JsonRequest,
        start: Slot,
        end: Slot,
        limit: Slot,
    ) -> HandlerResult {
        let blocks = self.engine.blocks();
        let end = end.min(blocks.latest().slot);
        let range = if start > end || limit == 0 {
            None
        } else {
            blocks.retained_range().map(|retained| {
                let start = start.max(*retained.start());
                let end = end.min(*retained.end()).min(start.saturating_add(limit - 1));
                start..=end
            })
        };
        ResponsePayload::encode_no_context(&request.id, BlockSlots(range))
    }
}

/// Serialize consecutive slots directly instead of materializing the range.
struct BlockSlots(Option<RangeInclusive<Slot>>);

impl Serialize for BlockSlots {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.0.clone().into_iter().flatten())
    }
}

/// Encode Engine's available block fields, retaining placeholders for unavailable metadata.
fn encode_block(
    response: BlockResponse,
    encoding: UiTransactionEncoding,
    options: BlockEncodingOptions,
) -> RpcResult<UiConfirmedBlock> {
    let block = *response.block();
    let (transactions, signatures) = match response {
        BlockResponse::Full(full) => (
            full.transactions
                .into_iter()
                .map(retained_transaction)
                .collect::<RpcResult<_>>()?,
            None,
        ),
        BlockResponse::WithSignatures(block) => (
            Vec::new(),
            Some(block.signatures.into_iter().map(|signature| signature.to_string()).collect()),
        ),
        BlockResponse::Bare(_) => (Vec::new(), None),
        BlockResponse::WithTransactions(_) => {
            return Err(RpcError::internal(
                "engine returned transaction-only block for an unsupported detail request",
            ));
        }
    };
    let mut encoded = ConfirmedBlock {
        previous_blockhash: block.parent.to_string(),
        blockhash: block.hash.to_string(),
        parent_slot: block.slot.saturating_sub(1),
        transactions,
        rewards: Vec::new(),
        num_partitions: None,
        block_time: Some(block.time),
        block_height: Some(block.slot),
    }
    .encode_with_options(encoding, options)
    .map_err(|error| RpcError::internal(format!("failed to encode engine block: {error}")))?;
    // The signature-only response already carries the signatures separately.
    if signatures.is_some() {
        encoded.signatures = signatures;
    }
    Ok(encoded)
}
