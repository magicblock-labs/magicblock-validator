// NOTE: from rpc/src/rpc.rs :2791
use jsonrpc_core::{Error, Result};
use log::*;
use sleipnir_rpc_client_api::{
    config::RpcContextConfig, request::MAX_GET_SLOT_LEADERS,
};
use solana_sdk::{
    clock::Slot, commitment_config::CommitmentConfig,
    epoch_schedule::EpochSchedule,
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

    fn get_slot_leader(
        &self,
        meta: Self::Metadata,
        config: Option<RpcContextConfig>,
    ) -> Result<String> {
        debug!("get_slot_leader rpc request received");
        Ok(meta
            .get_slot_leader(config.unwrap_or_default())?
            .to_string())
    }

    fn get_slot_leaders(
        &self,
        meta: Self::Metadata,
        start_slot: Slot,
        limit: u64,
    ) -> Result<Vec<String>> {
        debug!(
            "get_slot_leaders rpc request received (start: {} limit: {})",
            start_slot, limit
        );

        let limit = limit as usize;
        if limit > MAX_GET_SLOT_LEADERS {
            return Err(Error::invalid_params(format!(
                "Invalid limit; max {MAX_GET_SLOT_LEADERS}"
            )));
        }

        Ok(meta
            .get_slot_leaders(start_slot, limit)?
            .into_iter()
            .map(|identity| identity.to_string())
            .collect())
    }
}
