mod size;
pub(crate) mod worker;

use crate::shared::account_state::{
    AccountSnapshot, CacheTaskInfoFetcher, TaskInfoFetcher, TaskInfoFetcherResult,
};
use mbv_core::intent::MagicIntentBundle;
use solana_pubkey::Pubkey;
use std::{collections::HashMap, sync::Arc};

/// Admission access for the syscall adapter; shares execution's nonce cache.
#[derive(Clone)]
pub struct IntentAdmission(pub(crate) Arc<CacheTaskInfoFetcher>);

impl IntentAdmission {
    pub async fn fetch_current_commit_nonces(
        &self,
        accounts: &[AccountSnapshot],
        min_context_slot: u64,
    ) -> TaskInfoFetcherResult<HashMap<Pubkey, u64>> {
        self.0.fetch_current_commit_nonces(accounts, min_context_slot).await
    }

    pub fn fits(&self, intent: &MagicIntentBundle) -> bool {
        size::IntentSizeValidator::fits(intent)
    }
}
