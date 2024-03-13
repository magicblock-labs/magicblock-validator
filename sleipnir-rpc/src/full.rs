use jsonrpc_core::{BoxFuture, Result};
use sleipnir_rpc_client_api::{
    config::{
        RpcBlocksConfigWrapper, RpcContextConfig, RpcEpochConfig,
        RpcRequestAirdropConfig, RpcSendTransactionConfig,
        RpcSignaturesForAddressConfig, RpcSimulateTransactionConfig,
    },
    response::{
        Response as RpcResponse, RpcBlockhash,
        RpcConfirmedTransactionStatusWithSignature, RpcContactInfo,
        RpcInflationReward, RpcPerfSample, RpcPrioritizationFee,
        RpcSimulateTransactionResult,
    },
};
use solana_sdk::{
    clock::{Slot, UnixTimestamp},
    commitment_config::CommitmentConfig,
};

use crate::{
    json_rpc_request_processor::JsonRpcRequestProcessor, traits::rpc_full::Full,
};

pub struct FullImpl;

#[allow(unused_variables)]
impl Full for FullImpl {
    type Metadata = JsonRpcRequestProcessor;

    fn get_inflation_reward(
        &self,
        meta: Self::Metadata,
        address_strs: Vec<String>,
        config: Option<RpcEpochConfig>,
    ) -> BoxFuture<Result<Vec<Option<RpcInflationReward>>>> {
        todo!()
    }

    fn get_cluster_nodes(
        &self,
        meta: Self::Metadata,
    ) -> Result<Vec<RpcContactInfo>> {
        todo!()
    }

    fn get_recent_performance_samples(
        &self,
        meta: Self::Metadata,
        limit: Option<usize>,
    ) -> Result<Vec<RpcPerfSample>> {
        todo!()
    }

    fn get_max_retransmit_slot(&self, meta: Self::Metadata) -> Result<Slot> {
        todo!()
    }

    fn get_max_shred_insert_slot(&self, meta: Self::Metadata) -> Result<Slot> {
        todo!()
    }

    fn request_airdrop(
        &self,
        meta: Self::Metadata,
        pubkey_str: String,
        lamports: u64,
        config: Option<RpcRequestAirdropConfig>,
    ) -> Result<String> {
        todo!()
    }

    fn send_transaction(
        &self,
        meta: Self::Metadata,
        data: String,
        config: Option<RpcSendTransactionConfig>,
    ) -> Result<String> {
        todo!()
    }

    fn simulate_transaction(
        &self,
        meta: Self::Metadata,
        data: String,
        config: Option<RpcSimulateTransactionConfig>,
    ) -> Result<RpcResponse<RpcSimulateTransactionResult>> {
        todo!()
    }

    fn minimum_ledger_slot(&self, meta: Self::Metadata) -> Result<Slot> {
        todo!()
    }

    fn get_block_time(
        &self,
        meta: Self::Metadata,
        slot: Slot,
    ) -> BoxFuture<Result<Option<UnixTimestamp>>> {
        todo!()
    }

    fn get_blocks(
        &self,
        meta: Self::Metadata,
        start_slot: Slot,
        config: Option<RpcBlocksConfigWrapper>,
        commitment: Option<CommitmentConfig>,
    ) -> BoxFuture<Result<Vec<Slot>>> {
        todo!()
    }

    fn get_blocks_with_limit(
        &self,
        meta: Self::Metadata,
        start_slot: Slot,
        limit: usize,
        commitment: Option<CommitmentConfig>,
    ) -> BoxFuture<Result<Vec<Slot>>> {
        todo!()
    }

    fn get_signatures_for_address(
        &self,
        meta: Self::Metadata,
        address: String,
        config: Option<RpcSignaturesForAddressConfig>,
    ) -> BoxFuture<Result<Vec<RpcConfirmedTransactionStatusWithSignature>>>
    {
        todo!()
    }

    fn get_first_available_block(
        &self,
        meta: Self::Metadata,
    ) -> BoxFuture<Result<Slot>> {
        todo!()
    }

    fn get_latest_blockhash(
        &self,
        meta: Self::Metadata,
        config: Option<RpcContextConfig>,
    ) -> Result<RpcResponse<RpcBlockhash>> {
        todo!()
    }

    fn is_blockhash_valid(
        &self,
        meta: Self::Metadata,
        blockhash: String,
        config: Option<RpcContextConfig>,
    ) -> Result<RpcResponse<bool>> {
        todo!()
    }

    fn get_fee_for_message(
        &self,
        meta: Self::Metadata,
        data: String,
        config: Option<RpcContextConfig>,
    ) -> Result<RpcResponse<Option<u64>>> {
        todo!()
    }

    fn get_stake_minimum_delegation(
        &self,
        meta: Self::Metadata,
        config: Option<RpcContextConfig>,
    ) -> Result<RpcResponse<u64>> {
        todo!()
    }

    fn get_recent_prioritization_fees(
        &self,
        meta: Self::Metadata,
        pubkey_strs: Option<Vec<String>>,
    ) -> Result<Vec<RpcPrioritizationFee>> {
        todo!()
    }
}
