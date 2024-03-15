// NOTE: from rpc/src/rpc.rs
use jsonrpc_core::Result;
use jsonrpc_derive::rpc;
use sleipnir_rpc_client_api::{
    config::RpcContextConfig,
    response::Response as RpcResponse,
    response::{RpcIdentity, RpcSnapshotSlotInfo, RpcVersionInfo},
};
use solana_sdk::{epoch_info::EpochInfo, slot_history::Slot};

#[rpc]
pub trait Minimal {
    type Metadata;

    #[rpc(meta, name = "getBalance")]
    fn get_balance(
        &self,
        meta: Self::Metadata,
        pubkey_str: String,
        config: Option<RpcContextConfig>,
    ) -> Result<RpcResponse<u64>>;

    #[rpc(meta, name = "getEpochInfo")]
    fn get_epoch_info(
        &self,
        meta: Self::Metadata,
        config: Option<RpcContextConfig>,
    ) -> Result<EpochInfo>;

    #[rpc(meta, name = "getGenesisHash")]
    fn get_genesis_hash(&self, meta: Self::Metadata) -> Result<String>;

    #[rpc(meta, name = "getHealth")]
    fn get_health(&self, meta: Self::Metadata) -> Result<String>;

    #[rpc(meta, name = "getIdentity")]
    fn get_identity(&self, meta: Self::Metadata) -> Result<RpcIdentity>;

    #[rpc(meta, name = "getSlot")]
    fn get_slot(
        &self,
        meta: Self::Metadata,
        config: Option<RpcContextConfig>,
    ) -> Result<Slot>;

    #[rpc(meta, name = "getBlockHeight")]
    fn get_block_height(
        &self,
        meta: Self::Metadata,
        config: Option<RpcContextConfig>,
    ) -> Result<u64>;

    #[rpc(meta, name = "getHighestSnapshotSlot")]
    fn get_highest_snapshot_slot(
        &self,
        meta: Self::Metadata,
    ) -> Result<RpcSnapshotSlotInfo>;

    #[rpc(meta, name = "getTransactionCount")]
    fn get_transaction_count(
        &self,
        meta: Self::Metadata,
        config: Option<RpcContextConfig>,
    ) -> Result<u64>;

    #[rpc(meta, name = "getVersion")]
    fn get_version(&self, meta: Self::Metadata) -> Result<RpcVersionInfo>;
}
