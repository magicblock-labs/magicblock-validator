// NOTE: from rpc/src/rpc.rs :3432
use jsonrpc_core::{BoxFuture, Result};
use log::*;
use sleipnir_rpc_client_api::{
    config::{
        RpcBlocksConfigWrapper, RpcContextConfig, RpcEpochConfig,
        RpcRequestAirdropConfig, RpcSendTransactionConfig,
        RpcSignaturesForAddressConfig,
    },
    response::{
        Response as RpcResponse, RpcBlockhash,
        RpcConfirmedTransactionStatusWithSignature, RpcContactInfo,
        RpcInflationReward, RpcPerfSample, RpcPrioritizationFee,
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
        todo!("get_inflation_reward")
    }

    fn get_cluster_nodes(
        &self,
        meta: Self::Metadata,
    ) -> Result<Vec<RpcContactInfo>> {
        todo!("get_cluster_nodes")
    }

    fn get_recent_performance_samples(
        &self,
        meta: Self::Metadata,
        limit: Option<usize>,
    ) -> Result<Vec<RpcPerfSample>> {
        todo!("get_recent_performance_samples")
    }

    fn get_max_retransmit_slot(&self, meta: Self::Metadata) -> Result<Slot> {
        todo!("get_max_retransmit_slot")
    }

    fn get_max_shred_insert_slot(&self, meta: Self::Metadata) -> Result<Slot> {
        todo!("get_max_shred_insert_slot")
    }

    fn request_airdrop(
        &self,
        meta: Self::Metadata,
        pubkey_str: String,
        lamports: u64,
        config: Option<RpcRequestAirdropConfig>,
    ) -> Result<String> {
        todo!("request_airdrop")
    }

    fn send_transaction(
        &self,
        meta: Self::Metadata,
        data: String,
        config: Option<RpcSendTransactionConfig>,
    ) -> Result<String> {
        todo!("send_transaction")
    }

    fn minimum_ledger_slot(&self, meta: Self::Metadata) -> Result<Slot> {
        todo!("minimum_ledger_slot")
    }

    fn get_block_time(
        &self,
        meta: Self::Metadata,
        slot: Slot,
    ) -> BoxFuture<Result<Option<UnixTimestamp>>> {
        todo!("get_block_time")
    }

    fn get_blocks(
        &self,
        meta: Self::Metadata,
        start_slot: Slot,
        config: Option<RpcBlocksConfigWrapper>,
        commitment: Option<CommitmentConfig>,
    ) -> BoxFuture<Result<Vec<Slot>>> {
        todo!("get_blocks")
    }

    fn get_blocks_with_limit(
        &self,
        meta: Self::Metadata,
        start_slot: Slot,
        limit: usize,
        commitment: Option<CommitmentConfig>,
    ) -> BoxFuture<Result<Vec<Slot>>> {
        todo!("get_blocks_with_limit")
    }

    fn get_signatures_for_address(
        &self,
        meta: Self::Metadata,
        address: String,
        config: Option<RpcSignaturesForAddressConfig>,
    ) -> BoxFuture<Result<Vec<RpcConfirmedTransactionStatusWithSignature>>>
    {
        todo!("get_signatures_for_address")
    }

    fn get_first_available_block(
        &self,
        meta: Self::Metadata,
    ) -> BoxFuture<Result<Slot>> {
        Box::pin(async move { Ok(meta.get_first_available_block().await) })
    }

    fn get_latest_blockhash(
        &self,
        meta: Self::Metadata,
        _config: Option<RpcContextConfig>,
    ) -> Result<RpcResponse<RpcBlockhash>> {
        debug!("get_latest_blockhash rpc request received");
        meta.get_latest_blockhash()
    }

    fn is_blockhash_valid(
        &self,
        meta: Self::Metadata,
        blockhash: String,
        config: Option<RpcContextConfig>,
    ) -> Result<RpcResponse<bool>> {
        todo!("is_blockhash_valid")
    }

    fn get_fee_for_message(
        &self,
        meta: Self::Metadata,
        data: String,
        config: Option<RpcContextConfig>,
    ) -> Result<RpcResponse<Option<u64>>> {
        todo!("get_fee_for_message")
    }

    fn get_stake_minimum_delegation(
        &self,
        meta: Self::Metadata,
        config: Option<RpcContextConfig>,
    ) -> Result<RpcResponse<u64>> {
        todo!("get_stake_minimum_delegation")
    }

    fn get_recent_prioritization_fees(
        &self,
        meta: Self::Metadata,
        pubkey_strs: Option<Vec<String>>,
    ) -> Result<Vec<RpcPrioritizationFee>> {
        todo!("get_recent_prioritization_fees")
    }
}
