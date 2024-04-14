// NOTE: from rpc/src/rpc.rs :2791
use jsonrpc_core::Result;
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
        _commitment: Option<CommitmentConfig>,
    ) -> Result<u64> {
        debug!("get_minimum_balance_for_rent_exemption rpc request received");
        meta.get_minimum_balance_for_rent_exemption(data_len)
    }

    fn get_epoch_schedule(
        &self,
        meta: Self::Metadata,
    ) -> Result<EpochSchedule> {
        debug!("get_epoch_schedule rpc request received");
        Ok(meta.get_epoch_schedule())
    }
}
