use conjunto_transwise::AccountChainSnapshotShared;
use futures_util::future::BoxFuture;
use solana_sdk::pubkey::Pubkey;

// The result type must be clonable (we use a string as clonable error)
pub type AccountFetcherResult = Result<AccountChainSnapshotShared, String>;

pub trait AccountFetcher {
    fn fetch_account_chain_snapshot(
        &self,
        pubkey: &Pubkey,
    ) -> BoxFuture<AccountFetcherResult>;
}
