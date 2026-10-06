use nucleus::ledger::Block;
use solana_rpc_client_api::response::RpcBlockhash;

use super::{HandlerResult, RpcHandlers};
use crate::requests::{JsonHttpRequest as JsonRequest, payload::ResponsePayload};

const SOLANA_BLOCK_TIME_MS: f64 = 400.0;
const MAX_VALID_BLOCKHASH_SLOTS: f64 = 150.0;

impl RpcHandlers {
    pub(super) fn blockhash_response(&self, block: Block) -> RpcBlockhash {
        let ratio = SOLANA_BLOCK_TIME_MS / self.blocktime_ms.max(1) as f64;
        let validity = (ratio * MAX_VALID_BLOCKHASH_SLOTS) as u64;
        RpcBlockhash {
            blockhash: block.hash.to_string(),
            last_valid_block_height: block.slot + validity,
        }
    }

    pub(crate) fn get_latest_blockhash(&self, request: &JsonRequest) -> HandlerResult {
        let block = self.engine.blocks().latest();
        ResponsePayload::encode(&request.id, self.blockhash_response(block), block.slot)
    }
}
