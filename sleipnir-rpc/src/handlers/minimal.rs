// NOTE: from rpc/src/rpc.rs
use jsonrpc_core::Result;
use log::*;
use sleipnir_rpc_client_api::{
    config::{RpcContextConfig, RpcGetVoteAccountsConfig},
    response::{
        Response as RpcResponse, RpcIdentity, RpcSnapshotSlotInfo,
        RpcVersionInfo, RpcVoteAccountStatus,
    },
};
use solana_sdk::{epoch_info::EpochInfo, slot_history::Slot};

use crate::{
    json_rpc_request_processor::JsonRpcRequestProcessor,
    traits::rpc_minimal::Minimal,
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
        todo!("get_genesis_hash")
    }

    fn get_health(&self, meta: Self::Metadata) -> Result<String> {
        todo!("get_health")
    }

    fn get_identity(&self, meta: Self::Metadata) -> Result<RpcIdentity> {
        todo!("get_identity")
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
        todo!("get_highest_snapshot_slot")
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
}
