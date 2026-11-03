//! Test utilities for magicblock-committor-service tests

use std::collections::HashMap;

use delegation_program_api::state::{DelegationMetadata, UndelegationRequester};
use solana_account::Account;
use solana_pubkey::Pubkey;

use crate::shared::account_state::{AccountSnapshot, TaskInfoProvider, TaskInfoProviderResult};

#[derive(Default)]
pub(crate) struct MockInfoProvider {
    pub default_commit_id: u64,
    pub commit_ids: HashMap<Pubkey, u64>,
    pub next_commit_id: Option<u64>,
    pub delegation_metadata: HashMap<Pubkey, (UndelegationRequester, Pubkey)>,
    /// `None` allows a lookup that intentionally returns no account.
    pub base_accounts: HashMap<Pubkey, Option<Account>>,
}

#[async_trait::async_trait]
impl TaskInfoProvider for MockInfoProvider {
    async fn fetch_next_commit_nonces(
        &self,
        accounts: &[AccountSnapshot],
        _: u64,
    ) -> TaskInfoProviderResult<HashMap<Pubkey, u64>> {
        let id = self
            .next_commit_id
            .expect("unexpected next-ID request; the cache allocates IDs");
        Ok(accounts.iter().map(|(pubkey, _)| (*pubkey, id)).collect())
    }

    async fn fetch_current_commit_nonces(
        &self,
        accounts: &[AccountSnapshot],
        _: u64,
    ) -> TaskInfoProviderResult<HashMap<Pubkey, u64>> {
        Ok(accounts
            .iter()
            .map(|(pubkey, _)| {
                (
                    *pubkey,
                    self.commit_ids.get(pubkey).copied().unwrap_or(self.default_commit_id),
                )
            })
            .collect())
    }

    async fn fetch_delegation_metadata(
        &self,
        accounts: &[AccountSnapshot],
        _: u64,
    ) -> TaskInfoProviderResult<HashMap<Pubkey, DelegationMetadata>> {
        Ok(accounts
            .iter()
            .map(|(pubkey, _)| {
                let (undelegation_requester, rent_payer) = self
                    .delegation_metadata
                    .get(pubkey)
                    .copied()
                    .unwrap_or((UndelegationRequester::None, *pubkey));
                (
                    *pubkey,
                    DelegationMetadata {
                        last_commit_id: self.default_commit_id,
                        undelegation_requester,
                        seeds: vec![],
                        rent_payer,
                    },
                )
            })
            .collect())
    }

    async fn get_base_accounts(
        &self,
        pubkeys: &[Pubkey],
        _: u64,
    ) -> TaskInfoProviderResult<HashMap<Pubkey, Account>> {
        Ok(pubkeys
            .iter()
            .filter_map(|pubkey| {
                self.base_accounts
                    .get(pubkey)
                    .expect("unexpected base-account request")
                    .clone()
                    .map(|account| (*pubkey, account))
            })
            .collect())
    }
}

/// Initialize logging for tests. Call this at the start of each test
/// to enable structured logging via tracing.
///
/// This respects the RUST_LOG environment variable.
/// Example: `RUST_LOG=trace cargo test`
pub(crate) fn init_test_logger() {
    // Repeated across tests in a process, so failures to install are expected
    // and ignored: the first caller wins.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_test_writer()
        .try_init();
}
