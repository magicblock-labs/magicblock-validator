use magicblock_core::link::blocks::BlockHash;
use solana_account_decoder::parse_token::UiTokenAmount;
use solana_rpc_client_api::response::{
    RpcBlockCommitment, RpcContactInfo, RpcSnapshotSlotInfo, RpcSupply,
};

use super::prelude::*;

impl HttpDispatcher {
    pub(crate) fn get_slot_leader(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
        Ok(ResponsePayload::encode_no_context(
            &request.id,
            Serde32Bytes::from(self.identity),
        ))
    }

    pub(crate) fn get_slot_leaders(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
        Ok(ResponsePayload::encode_no_context(
            &request.id,
            [Serde32Bytes::from(self.identity)],
        ))
    }

    pub(crate) fn get_first_available_block(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
        Ok(ResponsePayload::encode_no_context(&request.id, 0))
    }

    pub(crate) fn get_largest_accounts(
        &self,
        request: &JsonRequest,
    ) -> HandlerResult {
        Ok(ResponsePayload::encode(
            &request.id,
            Vec::<()>::new(),
            self.blocks.get_latest().slot,
        ))
    }
    pub(crate) fn get_token_largest_accounts(
        &self,
        request: &JsonRequest,
    ) -> HandlerResult {
        Ok(ResponsePayload::encode(
            &request.id,
            Vec::<()>::new(),
            self.blocks.get_latest().slot,
        ))
    }

    pub(crate) fn get_token_supply(
        &self,
        request: &JsonRequest,
    ) -> HandlerResult {
        let supply = UiTokenAmount {
            ui_amount: None,
            decimals: 0,
            amount: String::new(),
            ui_amount_string: String::new(),
        };
        Ok(ResponsePayload::encode(
            &request.id,
            supply,
            self.blocks.get_latest().slot,
        ))
    }

    pub(crate) fn get_supply(&self, request: &JsonRequest) -> HandlerResult {
        let supply = RpcSupply {
            total: 0,
            non_circulating: 0,
            non_circulating_accounts: vec![],
            circulating: 0,
        };
        Ok(ResponsePayload::encode(
            &request.id,
            supply,
            self.blocks.get_latest().slot,
        ))
    }

    pub(crate) fn get_highest_snapshot_slot(
        &self,
        request: &JsonRequest,
    ) -> HandlerResult {
        let info = RpcSnapshotSlotInfo {
            full: 0,
            incremental: None,
        };
        Ok(ResponsePayload::encode_no_context(&request.id, info))
    }

    pub(crate) fn get_health(&self, request: &JsonRequest) -> HandlerResult {
        Ok(ResponsePayload::encode_no_context(&request.id, "ok"))
    }

    pub(crate) fn get_genesis_hash(
        &self,
        request: &JsonRequest,
    ) -> HandlerResult {
        Ok(ResponsePayload::encode_no_context(
            &request.id,
            BlockHash::default(),
        ))
    }

    pub(crate) fn get_epoch_info(
        &self,
        request: &JsonRequest,
    ) -> HandlerResult {
        let info = json::json! {{
            "epoch": 0,
            "slotIndex": 0,
            "slotsInEpoch": 0,
            "absoluteSlot": 0,
            "blockHeight": 0,
            "transactionCount": Some(0),
        }};
        Ok(ResponsePayload::encode_no_context(&request.id, info))
    }

    pub(crate) fn get_epoch_schedule(
        &self,
        request: &JsonRequest,
    ) -> HandlerResult {
        let schedule = json::json! {{
            "firstNormalEpoch": 0,
            "firstNormalSlot": 0,
            "leaderScheduleSlotOffset": 0,
            "slotsPerEpoch": 0,
            "warmup": true
        }};
        Ok(ResponsePayload::encode_no_context(&request.id, schedule))
    }

    pub(crate) fn get_block_commitment(
        &self,
        request: &JsonRequest,
    ) -> HandlerResult {
        let response = RpcBlockCommitment {
            commitment: Some([0; 32]),
            total_stake: 0,
        };
        Ok(ResponsePayload::encode_no_context(&request.id, response))
    }

    pub(crate) fn get_cluster_nodes(
        &self,
        request: &JsonRequest,
    ) -> HandlerResult {
        let info = RpcContactInfo {
            pubkey: self.identity.to_string(),
            gossip: None,
            tvu: None,
            tpu: None,
            tpu_quic: None,
            tpu_forwards: None,
            tpu_forwards_quic: None,
            tpu_vote: None,
            serve_repair: None,
            rpc: None,
            pubsub: None,
            version: None,
            shred_version: None,
            feature_set: None,
        };
        Ok(ResponsePayload::encode_no_context(&request.id, [info]))
    }
}
