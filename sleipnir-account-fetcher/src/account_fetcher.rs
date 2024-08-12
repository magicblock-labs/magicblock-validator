use async_trait::async_trait;
use conjunto_transwise::AccountChainSnapshotShared;
use solana_sdk::pubkey::Pubkey;

// The result type must be clonable (we use a string as error)
pub type AccountFetcherResult = Result<AccountChainSnapshotShared, String>;

#[async_trait]
pub trait AccountFetcher {
    async fn get_or_fetch_account_chain_snapshot(
        &self,
        pubkey: &Pubkey,
    ) -> AccountFetcherResult;
}
