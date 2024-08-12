use async_trait::async_trait;
use conjunto_transwise::AccountChainSnapshotShared;
use solana_sdk::pubkey::Pubkey;

// The result type must be clonable (we use a string as error)
pub type AccountFetcherResult = Result<AccountChainSnapshotShared, String>;

#[async_trait]
pub trait AccountFetcher {
    fn get_last_account_chain_snapshot(
        &self,
        pubkey: &Pubkey,
    ) -> Option<AccountFetcherResult>;
    async fn fetch_account_chain_snapshot(
        &self,
        pubkey: &Pubkey,
    ) -> AccountFetcherResult;
}
