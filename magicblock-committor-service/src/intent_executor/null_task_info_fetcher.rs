use std::collections::HashMap;

use async_trait::async_trait;
use magicblock_rpc_client::MagicBlockRpcClientResult;
use solana_account::Account;
use solana_pubkey::Pubkey;

use super::task_info_fetcher::{
    ResetType, TaskInfoFetcher, TaskInfoFetcherResult,
};

pub struct NullTaskInfoFetcher;

#[async_trait]
impl TaskInfoFetcher for NullTaskInfoFetcher {
    async fn fetch_next_commit_ids(
        &self,
        _pubkeys: &[Pubkey],
    ) -> TaskInfoFetcherResult<HashMap<Pubkey, u64>> {
        Ok(Default::default())
    }

    async fn fetch_rent_reimbursements(
        &self,
        _pubkeys: &[Pubkey],
    ) -> TaskInfoFetcherResult<Vec<Pubkey>> {
        Ok(Default::default())
    }

    fn peek_commit_id(&self, _pubkey: &Pubkey) -> Option<u64> {
        None
    }

    fn reset(&self, _: ResetType) {}

    async fn get_base_account(
        &self,
        _pubkey: &Pubkey,
    ) -> MagicBlockRpcClientResult<Option<Account>> {
        Ok(None) // AccountNotFound
    }
}
