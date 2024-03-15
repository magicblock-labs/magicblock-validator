// NOTE: from rpc/src/rpc.rs :2791
use jsonrpc_core::Result;
use jsonrpc_derive::rpc;
use log::*;
use solana_sdk::{
    commitment_config::CommitmentConfig, epoch_schedule::EpochSchedule,
};

use crate::{
    json_rpc_request_processor::JsonRpcRequestProcessor,
    traits::rpc_bank_data::BankData,
};

pub struct BankDataImpl;
#[allow(unused)]
impl BankData for BankDataImpl {
    type Metadata = JsonRpcRequestProcessor;

    fn get_minimum_balance_for_rent_exemption(
        &self,
        meta: Self::Metadata,
        data_len: usize,
        commitment: Option<CommitmentConfig>,
    ) -> Result<u64> {
        todo!("get_minimum_balance_for_rent_exemption")
    }

    fn get_epoch_schedule(
        &self,
        meta: Self::Metadata,
    ) -> Result<EpochSchedule> {
        debug!("get_epoch_schedule rpc request received");
        Ok(meta.get_epoch_schedule())
    }
}
