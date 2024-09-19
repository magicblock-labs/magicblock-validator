use std::collections::HashSet;

use conjunto_transwise::AccountChainSnapshotShared;
use futures_util::future::BoxFuture;
use sleipnir_account_dumper::AccountDumperError;
use sleipnir_account_fetcher::AccountFetcherError;
use sleipnir_account_updates::AccountUpdatesError;
use solana_sdk::{
    clock::Slot,
    compute_budget,
    pubkey::Pubkey,
    signature::Signature,
    sysvar::{
        clock, epoch_rewards, epoch_schedule, fees, instructions,
        last_restart_slot, recent_blockhashes, rent, rewards, slot_hashes,
        slot_history, stake_history,
    },
};
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
    DisallowNewAccount,
    DisallowPayerAccount,
    DisallowPdaAccount,
    DisallowDelegatedAccount,
    DisallowProgramAccount,
}

#[derive(Debug, Clone)]
pub struct AccountClonerPermissions {
    pub allow_cloning_refresh: bool,
    pub allow_cloning_new_accounts: bool,
    pub allow_cloning_payer_accounts: bool,
    pub allow_cloning_pda_accounts: bool,
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
    blacklisted_accounts.insert(clock::ID);
    blacklisted_accounts.insert(epoch_rewards::ID);
    blacklisted_accounts.insert(epoch_schedule::ID);
    blacklisted_accounts.insert(fees::ID);
    blacklisted_accounts.insert(instructions::ID);
    blacklisted_accounts.insert(last_restart_slot::ID);
    blacklisted_accounts.insert(recent_blockhashes::ID);
    blacklisted_accounts.insert(rent::ID);
    blacklisted_accounts.insert(rewards::ID);
    blacklisted_accounts.insert(slot_hashes::ID);
    blacklisted_accounts.insert(slot_history::ID);
    blacklisted_accounts.insert(stake_history::ID);
    blacklisted_accounts.insert(compute_budget::ID);
    blacklisted_accounts.insert(*validator_id);
    blacklisted_accounts
}
