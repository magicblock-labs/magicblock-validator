mod intent_limits;
pub(crate) mod worker;

pub use intent_limits::is_intent_within_limits;

use crate::shared::account_state::{
    AccountSnapshot, CachedTaskInfoProvider, TaskInfoProvider, TaskInfoProviderResult,
};
use solana_pubkey::Pubkey;
use std::{collections::HashMap, sync::Arc};

/// Thin wrapper exposed outside the committor (e.g. to the `MagicSys` adapter)
/// for reading current commit IDs. Shares execution's cache and exposes no other
/// provider operations.
#[derive(Clone)]
pub struct CommitIdProvider(pub(crate) Arc<CachedTaskInfoProvider>);

impl CommitIdProvider {
    pub async fn current_commit_ids(
        &self,
        accounts: &[AccountSnapshot],
        min_context_slot: u64,
    ) -> TaskInfoProviderResult<HashMap<Pubkey, u64>> {
        self.0.fetch_current_commit_nonces(accounts, min_context_slot).await
    }
}
