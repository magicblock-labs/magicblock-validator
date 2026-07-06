//! # Mocked Solana RPC Method Implementations
//!
//! This module provides mocked or placeholder implementations for a subset of the
//! Solana JSON-RPC API.
//!
//! These handlers are designed for a magicblock validator that does not track the
//! extensive state required to fully answer these queries (e.g., epoch schedules,
//! full supply details). They ensure API compatibility with standard tools by
//! returning default or empty responses, rather than 'method not found' errors.

use solana_account_decoder::parse_token::UiTokenAmount;
use solana_rpc_client_api::response::{
    RpcBlockCommitment, RpcContactInfo, RpcSnapshotSlotInfo, RpcSupply,
    RpcVoteAccountStatus,
};

use super::HandlerResult;
use crate::{
    requests::{
        JsonHttpRequest as JsonRequest, params::Serde32Bytes,
        payload::ResponsePayload,
    },
    server::http::dispatch::HttpDispatcher,
};
const SLOTS_IN_EPOCH: u64 = 432_000;

impl HttpDispatcher {
    /// Handles the `getSlotLeader` RPC request.
    /// This is a **mocked implementation** that always returns the validator's own
    /// identity as the current slot leader.
    pub(crate) fn get_slot_leader(
        &self,
        request: &JsonRequest,
    ) -> HandlerResult {
        Ok(ResponsePayload::encode_no_context(
            &request.id,
            Serde32Bytes::from(self.engine.authority()),
        ))
    }

    pub(crate) fn mock_zero(&self, request: &JsonRequest) -> HandlerResult {
        Ok(ResponsePayload::encode_no_context(&request.id, 0u64))
    }

    /// Handles the `getSlotLeaders` RPC request.
    /// This is a **mocked implementation** that always returns a list containing
    /// only the validator's own identity.
    pub(crate) fn get_slot_leaders(
        &self,
        request: &JsonRequest,
    ) -> HandlerResult {
        Ok(ResponsePayload::encode_no_context(
            &request.id,
            [Serde32Bytes::from(self.engine.authority())],
        ))
    }

    pub(crate) fn mock_empty_context(
        &self,
        request: &JsonRequest,
    ) -> HandlerResult {
        Ok(ResponsePayload::encode(
            &request.id,
            Vec::<()>::new(),
            self.engine.blocks().latest().slot,
        ))
    }

    /// Handles the `getTokenSupply` RPC request.
    /// This is a **mocked implementation** that returns an empty token supply struct.
    pub(crate) fn get_token_supply(
        &self,
        request: &JsonRequest,
    ) -> HandlerResult {
        let supply = UiTokenAmount {
            ui_amount: Some(0.0),
            decimals: 0,
            amount: "0".into(),
            ui_amount_string: "0.0".into(),
        };
        Ok(ResponsePayload::encode(
            &request.id,
            supply,
            self.engine.blocks().latest().slot,
        ))
    }

    /// Handles the `getSupply` RPC request.
    /// This is a **mocked implementation** that returns a
    /// supply struct with all values set to hardcoded values.
    pub(crate) fn get_supply(&self, request: &JsonRequest) -> HandlerResult {
        let supply = RpcSupply {
            total: u64::MAX,
            non_circulating: u64::MAX / 2,
            non_circulating_accounts: vec![],
            circulating: u64::MAX / 2,
        };
        Ok(ResponsePayload::encode(
            &request.id,
            supply,
            self.engine.blocks().latest().slot,
        ))
    }

    /// Handles the `getHighestSnapshotSlot` RPC request.
    /// This is a **mocked implementation** that returns a default snapshot info struct.
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

    /// Handles the `getHealth` RPC request.
    /// Returns a simple `"ok"` status to indicate that the RPC endpoint is reachable.
    pub(crate) fn get_health(&self, request: &JsonRequest) -> HandlerResult {
        Ok(ResponsePayload::encode_no_context(&request.id, "ok"))
    }

    /// Handles the `getGenesisHash` RPC request.
    /// This is a **placeholder implementation** that returns a default hash.
    pub(crate) fn get_genesis_hash(
        &self,
        request: &JsonRequest,
    ) -> HandlerResult {
        Ok(ResponsePayload::encode_no_context(
            &request.id,
            Serde32Bytes::from(solana_hash::Hash::default()),
        ))
    }

    /// Handles the `getEpochInfo` RPC request.
    /// This is a **mocked implementation** that returns a default epoch info object.
    pub(crate) fn get_epoch_info(
        &self,
        request: &JsonRequest,
    ) -> HandlerResult {
        let slot = self.engine.blocks().latest().slot;
        let info = json::json! {{
            "epoch": slot / SLOTS_IN_EPOCH,
            "slotIndex": slot % SLOTS_IN_EPOCH,
            "slotsInEpoch": SLOTS_IN_EPOCH,
            "absoluteSlot": slot,
            "blockHeight": slot,
            "transactionCount": Some(0),
        }};
        Ok(ResponsePayload::encode_no_context(&request.id, info))
    }

    /// Handles the `getEpochSchedule` RPC request.
    /// This is a **mocked implementation** that returns a default epoch schedule object.
    pub(crate) fn get_epoch_schedule(
        &self,
        request: &JsonRequest,
    ) -> HandlerResult {
        let schedule = json::json! {{
            "firstNormalEpoch": 0,
            "firstNormalSlot": 0,
            "leaderScheduleSlotOffset": 0,
            "slotsPerEpoch": SLOTS_IN_EPOCH,
            "warmup": false
        }};
        Ok(ResponsePayload::encode_no_context(&request.id, schedule))
    }

    /// Handles the `getBlockCommitment` RPC request.
    /// This is a **mocked implementation** that returns a default block commitment object.
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

    /// Handles the `getClusterNodes` RPC request.
    /// This is a **mocked implementation** that returns a list containing only this
    /// validator's contact information.
    pub(crate) fn get_cluster_nodes(
        &self,
        request: &JsonRequest,
    ) -> HandlerResult {
        let info = RpcContactInfo {
            pubkey: self.engine.authority().to_string(),
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
            client_id: None,
            shred_version: None,
            feature_set: None,
        };
        Ok(ResponsePayload::encode_no_context(&request.id, [info]))
    }

    pub(crate) fn get_vote_accounts(
        &self,
        request: &JsonRequest,
    ) -> HandlerResult {
        let status = RpcVoteAccountStatus {
            current: Vec::new(),
            delinquent: Vec::new(),
        };
        Ok(ResponsePayload::encode_no_context(&request.id, status))
    }

    pub(crate) fn mock_empty(&self, request: &JsonRequest) -> HandlerResult {
        Ok(ResponsePayload::encode_no_context(
            &request.id,
            Vec::<()>::new(),
        ))
    }
}
