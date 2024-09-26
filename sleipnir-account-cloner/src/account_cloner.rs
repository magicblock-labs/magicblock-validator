use std::collections::HashSet;

use conjunto_transwise::AccountChainSnapshotShared;
use futures_util::future::BoxFuture;
use sleipnir_account_dumper::AccountDumperError;
use sleipnir_account_fetcher::AccountFetcherError;
use sleipnir_account_updates::AccountUpdatesError;
use solana_sdk::{clock::Slot, pubkey::Pubkey, signature::Signature};
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
pub enum AccountClonerUnclonableReason {
    AlreadyLocallyOverriden,
    NoCloningAllowed,
    IsBlacklisted,
    IsNotAllowedProgram,
    DisallowWalletAccount,
    DisallowUndelegatedAccount,
    DisallowDelegatedAccount,
    DisallowProgramAccount,
}

#[derive(Debug, Clone)]
pub struct AccountClonerPermissions {
    pub allow_cloning_refresh: bool,
    pub allow_cloning_wallet_accounts: bool,
    pub allow_cloning_undelegated_accounts: bool,
    pub allow_cloning_delegated_accounts: bool,
    pub allow_cloning_program_accounts: bool,
}

#[derive(Debug, Clone)]
pub enum AccountClonerOutput {
    Cloned {
        account_chain_snapshot: AccountChainSnapshotShared,
        signature: Signature,
    },
    Unclonable {
        pubkey: Pubkey,
        reason: AccountClonerUnclonableReason,
        at_slot: Slot,
    },
}

pub trait AccountCloner {
    fn clone_account(
        &self,
        pubkey: &Pubkey,
    ) -> BoxFuture<AccountClonerResult<AccountClonerOutput>>;
}

pub fn standard_blacklisted_accounts(validator_id: &Pubkey) -> HashSet<Pubkey> {
    let mut blacklisted_accounts = HashSet::new();
    blacklisted_accounts.insert(solana_sdk::system_program::ID);
    blacklisted_accounts.insert(solana_sdk::compute_budget::ID);
    blacklisted_accounts.insert(solana_sdk::native_loader::ID);
    blacklisted_accounts.insert(solana_sdk::bpf_loader::ID);
    blacklisted_accounts.insert(solana_sdk::bpf_loader_deprecated::ID);
    blacklisted_accounts.insert(solana_sdk::bpf_loader_upgradeable::ID);
    blacklisted_accounts.insert(solana_sdk::loader_v4::ID);
    blacklisted_accounts.insert(solana_sdk::incinerator::ID);
    blacklisted_accounts.insert(solana_sdk::secp256k1_program::ID);
    blacklisted_accounts.insert(solana_sdk::ed25519_program::ID);
    blacklisted_accounts.insert(solana_sdk::address_lookup_table::program::ID);
    blacklisted_accounts.insert(solana_sdk::config::program::ID);
    blacklisted_accounts.insert(solana_sdk::stake::program::ID);
    blacklisted_accounts.insert(solana_sdk::stake::config::ID);
    blacklisted_accounts.insert(solana_sdk::vote::program::ID);
    blacklisted_accounts.insert(solana_sdk::feature::ID);
    blacklisted_accounts.insert(solana_sdk::sysvar::ID);
    blacklisted_accounts.insert(solana_sdk::sysvar::clock::ID);
    blacklisted_accounts.insert(solana_sdk::sysvar::epoch_rewards::ID);
    blacklisted_accounts.insert(solana_sdk::sysvar::epoch_schedule::ID);
    blacklisted_accounts.insert(solana_sdk::sysvar::fees::ID);
    blacklisted_accounts.insert(solana_sdk::sysvar::instructions::ID);
    blacklisted_accounts.insert(solana_sdk::sysvar::last_restart_slot::ID);
    blacklisted_accounts.insert(solana_sdk::sysvar::recent_blockhashes::ID);
    blacklisted_accounts.insert(solana_sdk::sysvar::rent::ID);
    blacklisted_accounts.insert(solana_sdk::sysvar::rewards::ID);
    blacklisted_accounts.insert(solana_sdk::sysvar::slot_hashes::ID);
    blacklisted_accounts.insert(solana_sdk::sysvar::slot_history::ID);
    blacklisted_accounts.insert(solana_sdk::sysvar::stake_history::ID);
    blacklisted_accounts.insert(*validator_id);
    blacklisted_accounts
}
