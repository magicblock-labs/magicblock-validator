use magicblock_core::Slot;

use super::HandlerResult;
use crate::{
    error::RpcError,
    requests::{JsonHttpRequest as JsonRequest, payload::ResponsePayload},
    server::http::dispatch::HttpDispatcher,
};

const MAX_BLOCKS: u64 = 500_000;

#[derive(Clone, Copy)]
pub(crate) enum BlockRange {
    EndSlot,
    Limit,
}

impl HttpDispatcher {
    pub(crate) fn get_blocks(
        &self,
        request: &JsonRequest,
        range: BlockRange,
    ) -> HandlerResult {
        let start = request.required::<Slot>(0)?;
        let latest = self.engine.blocks().latest().slot;
        let slots: Vec<Slot> = match range {
            BlockRange::EndSlot => {
                let end = request
                    .optional::<Slot>(1)?
                    .unwrap_or(latest)
                    .min(latest)
                    .min(start.saturating_add(MAX_BLOCKS));
                if start > end {
                    return Err(RpcError::invalid_params(
                        "start slot is greater than the end slot",
                    ));
                }
                (start..=end).collect()
            }
            BlockRange::Limit => {
                let limit = request.required::<Slot>(1)?.min(MAX_BLOCKS);
                let end =
                    start.saturating_add(limit).min(latest.saturating_add(1));
                (start..end).collect()
            }
        };
        Ok(ResponsePayload::encode_no_context(&request.id, slots))
    }
}
