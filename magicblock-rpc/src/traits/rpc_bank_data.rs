// NOTE: from rpc/src/rpc.rs :2741
use jsonrpc_core::Result;
use jsonrpc_derive::rpc;
use solana_rpc_client_api::config::RpcContextConfig;
use solana_sdk::{
    commitment_config::CommitmentConfig, epoch_schedule::EpochSchedule,
};

#[rpc]
pub trait BankData {
    type Metadata;

    #[rpc(meta, name = "getMinimumBalanceForRentExemption")]
    fn get_minimum_balance_for_rent_exemption(
        &self,
        meta: Self::Metadata,
        data_len: usize,
        commitment: Option<CommitmentConfig>,
    ) -> Result<u64>;

    /*
    #[rpc(meta, name = "getInflationGovernor")]
    fn get_inflation_governor(
        &self,
        meta: Self::Metadata,
        commitment: Option<CommitmentConfig>,
    ) -> Result<RpcInflationGovernor>;

    #[rpc(meta, name = "getInflationRate")]
    fn get_inflation_rate(
        &self,
        meta: Self::Metadata,
    ) -> Result<RpcInflationRate>;
    */

    #[rpc(meta, name = "getEpochSchedule")]
    fn get_epoch_schedule(&self, meta: Self::Metadata)
        -> Result<EpochSchedule>;

    #[rpc(meta, name = "getSlotLeader")]
    fn get_slot_leader(
        &self,
        meta: Self::Metadata,
        config: Option<RpcContextConfig>,
    ) -> Result<String>;

    #[rpc(meta, name = "getSlotLeaders")]
    fn get_slot_leaders(
        &self,
        meta: Self::Metadata,
        start_slot: solana_sdk::clock::Slot,
        limit: u64,
    ) -> Result<Vec<String>>;

    /*
    #[rpc(meta, name = "getBlockProduction")]
    fn get_block_production(
        &self,
        meta: Self::Metadata,
        config: Option<RpcBlockProductionConfig>,
    ) -> Result<RpcResponse<RpcBlockProduction>>;
    */
}
