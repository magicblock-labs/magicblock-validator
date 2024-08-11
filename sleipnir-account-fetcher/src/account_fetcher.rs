use async_trait::async_trait;
use solana_sdk::pubkey::Pubkey;

use crate::RemoteAccountFetcherResult;

#[async_trait]
pub trait AccountFetcher {
    async fn get_or_fetch_account_chain_snapshot(
        &self,
        pubkey: &Pubkey,
    ) -> RemoteAccountFetcherResult;
}
