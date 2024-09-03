use std::collections::HashSet;

use conjunto_transwise::AccountChainSnapshotShared;
use futures_util::future::BoxFuture;
use sleipnir_account_dumper::AccountDumperError;
use sleipnir_account_fetcher::AccountFetcherError;
use sleipnir_account_updates::AccountUpdatesError;
use solana_sdk::{compute_budget, pubkey::Pubkey, sysvar};
use thiserror::Error;
use tokio::sync::oneshot::Sender;

#[derive(Debug, Clone, Error)]
pub enum AccountClonerError {
    #[error(transparent)]
    SendError(#[from] tokio::sync::mpsc::error::SendError<Pubkey>),

    #[error(transparent)]
    RecvError(#[from] tokio::sync::oneshot::error::RecvError),

    #[error(transparent)]
    AccountFetcherError(#[from] AccountFetcherError),

    #[error(transparent)]
    AccountUpdatesError(#[from] AccountUpdatesError),

    #[error(transparent)]
    AccountDumperError(#[from] AccountDumperError),

    #[error("ProgramDataDoesNotExist")]
    ProgramDataDoesNotExist,
}

pub type AccountClonerResult<T> = Result<T, AccountClonerError>;

pub type AccountClonerListeners =
    Vec<Sender<AccountClonerResult<AccountClonerOutput>>>;

#[derive(Debug, Clone)]
pub enum AccountClonerOutput {
    Cloned(AccountChainSnapshotShared),
    Unclonable(Pubkey),
}

pub trait AccountCloner {
    fn clone_account(
        &self,
        pubkey: &Pubkey,
    ) -> BoxFuture<AccountClonerResult<AccountClonerOutput>>;
}

pub fn standard_blacklisted_accounts(validator_id: &Pubkey) -> HashSet<Pubkey> {
    let mut blacklisted_accounts = HashSet::new();
    blacklisted_accounts.insert(sysvar::clock::ID);
    blacklisted_accounts.insert(sysvar::epoch_rewards::ID);
    blacklisted_accounts.insert(sysvar::epoch_schedule::ID);
    blacklisted_accounts.insert(sysvar::fees::ID);
    blacklisted_accounts.insert(sysvar::instructions::ID);
    blacklisted_accounts.insert(sysvar::last_restart_slot::ID);
    blacklisted_accounts.insert(sysvar::recent_blockhashes::ID);
    blacklisted_accounts.insert(sysvar::rent::ID);
    blacklisted_accounts.insert(sysvar::rewards::ID);
    blacklisted_accounts.insert(sysvar::slot_hashes::ID);
    blacklisted_accounts.insert(sysvar::slot_history::ID);
    blacklisted_accounts.insert(sysvar::stake_history::ID);
    blacklisted_accounts.insert(compute_budget::ID);
    blacklisted_accounts.insert(*validator_id);
    blacklisted_accounts
}
