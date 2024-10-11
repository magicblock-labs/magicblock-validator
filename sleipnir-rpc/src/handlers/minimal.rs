// NOTE: from rpc/src/rpc.rs
use jsonrpc_core::Result;
use log::*;
use solana_rpc_client_api::{
    config::{
        RpcContextConfig, RpcGetVoteAccountsConfig, RpcLeaderScheduleConfig,
        RpcLeaderScheduleConfigWrapper,
    },
    custom_error::RpcCustomError,
    response::{
        Response as RpcResponse, RpcIdentity, RpcLeaderSchedule,
        RpcSnapshotSlotInfo, RpcVersionInfo, RpcVoteAccountStatus,
    },
};
use solana_sdk::{epoch_info::EpochInfo, slot_history::Slot};

use crate::{
    json_rpc_request_processor::JsonRpcRequestProcessor,
    rpc_health::RpcHealthStatus, traits::rpc_minimal::Minimal,
    utils::verify_pubkey,
};

pub struct MinimalImpl;
#[allow(unused)]
impl Minimal for MinimalImpl {
    type Metadata = JsonRpcRequestProcessor;

    fn get_balance(
        &self,
        meta: Self::Metadata,
        pubkey_str: String,
        _config: Option<RpcContextConfig>,
    ) -> Result<RpcResponse<u64>> {
        meta.get_balance(pubkey_str)
    }

    fn get_epoch_info(
        &self,
        meta: Self::Metadata,
        config: Option<RpcContextConfig>,
    ) -> Result<EpochInfo> {
        debug!("get_epoch_info rpc request received");
        let bank = meta.get_bank_with_config(config.unwrap_or_default())?;
        Ok(bank.get_epoch_info())
    }

    fn get_genesis_hash(&self, meta: Self::Metadata) -> Result<String> {
        debug!("get_genesis_hash rpc request received");
        Ok(meta.genesis_hash.to_string())
    }

    fn get_health(&self, meta: Self::Metadata) -> Result<String> {
        match meta.health.check() {
            RpcHealthStatus::Ok => Ok("ok".to_string()),
            RpcHealthStatus::Unknown => Err(RpcCustomError::NodeUnhealthy {
                num_slots_behind: None,
            }
            .into()),
        }
    }

    fn get_identity(&self, meta: Self::Metadata) -> Result<RpcIdentity> {
        debug!("get_identity rpc request received");
        let identity = meta.get_identity();
        Ok(RpcIdentity {
            identity: identity.to_string(),
        })
    }

    fn get_slot(
        &self,
        meta: Self::Metadata,
        config: Option<RpcContextConfig>,
    ) -> Result<Slot> {
        debug!("get_slot rpc request received");
        meta.get_slot(config.unwrap_or_default())
    }

    fn get_block_height(
        &self,
        meta: Self::Metadata,
        config: Option<RpcContextConfig>,
    ) -> Result<u64> {
        debug!("get_block_height rpc request received");
        meta.get_block_height(config.unwrap_or_default())
    }

    fn get_highest_snapshot_slot(
        &self,
        meta: Self::Metadata,
    ) -> Result<RpcSnapshotSlotInfo> {
        debug!("get_highest_snapshot_slot rpc request received");
        // We always start the validator on slot 0 and never clear or snapshot the history
        // There will be some related work here: https://github.com/magicblock-labs/magicblock-validator/issues/112
        Ok(RpcSnapshotSlotInfo {
            full: 0,
            incremental: None,
        })
    }

    fn get_transaction_count(
        &self,
        meta: Self::Metadata,
        config: Option<RpcContextConfig>,
    ) -> Result<u64> {
        debug!("get_transaction_count rpc request received");
        meta.get_transaction_count(config.unwrap_or_default())
    }

    fn get_vote_accounts(
        &self,
        meta: Self::Metadata,
        config: Option<RpcGetVoteAccountsConfig>,
    ) -> Result<RpcVoteAccountStatus> {
        Ok(RpcVoteAccountStatus {
            current: vec![],
            delinquent: vec![],
        })
    }

    fn get_version(&self, _: Self::Metadata) -> Result<RpcVersionInfo> {
        debug!("get_version rpc request received");
        let version = sleipnir_version::Version::default();
        Ok(RpcVersionInfo {
            solana_core: version.to_string(),
            feature_set: Some(version.feature_set),
        })
    }

    fn get_leader_schedule(
        &self,
        meta: Self::Metadata,
        options: Option<RpcLeaderScheduleConfigWrapper>,
        config: Option<RpcLeaderScheduleConfig>,
    ) -> Result<Option<RpcLeaderSchedule>> {
        let (slot, wrapped_config) =
            options.as_ref().map(|x| x.unzip()).unwrap_or_default();
        let config = wrapped_config.or(config).unwrap_or_default();

        let identity = meta.get_identity().to_string();

        if let Some(ref requested_identity) = config.identity {
            let _ = verify_pubkey(requested_identity)?;
            // We are the only leader around
            if requested_identity != &identity {
                return Ok(None);
            }
        }

        let bank = meta.get_bank();
        let slot = slot.unwrap_or_else(|| bank.slot());
        let epoch = bank.epoch_schedule().get_epoch(slot);
        let slots_in_epoch = bank.get_slots_in_epoch(epoch);

        // We are always the leader thus we add every slot in the epoch
        let slots = (0..slots_in_epoch as usize).collect::<Vec<_>>();
        let leader_schedule = [(identity, slots)].into();

        Ok(Some(leader_schedule))
    }
}
