use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, RwLock},
};

use conjunto_transwise::AccountChainSnapshotShared;
use futures_util::future::BoxFuture;
use magicblock_account_dumper::AccountDumperError;
use magicblock_account_fetcher::AccountFetcherError;
use magicblock_account_updates::AccountUpdatesError;
use magicblock_committor_service::{
    error::{CommittorServiceError, CommittorServiceResult},
    ChangesetCommittor,
};
use magicblock_core::magic_program;
use solana_sdk::{clock::Slot, pubkey::Pubkey, signature::Signature};
use thiserror::Error;
use tokio::sync::oneshot::{self, Sender};

#[derive(Debug, Clone, Error)]
pub enum AccountClonerError {
    #[error(transparent)]
    SendError(#[from] flume::SendError<Pubkey>),

    #[error(transparent)]
    RecvError(#[from] tokio::sync::oneshot::error::RecvError),

    #[error("JoinError ({0})")]
    JoinError(String),

    #[error(transparent)]
    AccountFetcherError(#[from] AccountFetcherError),

    #[error(transparent)]
    AccountUpdatesError(#[from] AccountUpdatesError),

    #[error(transparent)]
    AccountDumperError(#[from] AccountDumperError),

    #[error("CommittorServiceError {0}")]
    CommittorServiceError(String),

    #[error("ProgramDataDoesNotExist")]
    ProgramDataDoesNotExist,

    #[error("FailedToFetchSatisfactorySlot")]
    FailedToFetchSatisfactorySlot,

    #[error("FailedToGetSubscriptionSlot")]
    FailedToGetSubscriptionSlot,
}

pub type AccountClonerResult<T> = Result<T, AccountClonerError>;

pub type CloneOutputMap = Arc<RwLock<HashMap<Pubkey, AccountClonerOutput>>>;

pub type AccountClonerListeners =
    Vec<Sender<AccountClonerResult<AccountClonerOutput>>>;

#[derive(Debug, Clone)]
pub enum AccountClonerUnclonableReason {
    AlreadyLocallyOverriden,
    NoCloningAllowed,
    IsBlacklisted,
    IsNotAnAllowedProgram,
    DoesNotAllowFeePayerAccount,
    DoesNotAllowUndelegatedAccount,
    DoesNotAllowDelegatedAccount,
    DoesNotAllowProgramAccount,
    DoesNotHaveEscrowAccount,
    DoesNotHaveDelegatedEscrowAccount,
    DoesNotAllowEscrowedPda,
    DoesNotAllowFeepayerWithEscrowedPda,
    /// If an account is delegated to our validator then we should use the latest
    /// state in our own bank since that is more up to date than the on-chain state.
    DelegatedAccountsNotClonedWhileHydrating,
}

pub async fn map_committor_request_result<T, CC: ChangesetCommittor>(
    res: oneshot::Receiver<CommittorServiceResult<T>>,
    changeset_committor: Arc<CC>,
) -> AccountClonerResult<T> {
    match res.await.map_err(|err| {
        // Send request error
        AccountClonerError::CommittorServiceError(format!(
            "error sending request {err:?}"
        ))
    })? {
        Ok(val) => Ok(val),
        Err(err) => {
            // Commit error
            match err {
                CommittorServiceError::TableManiaError(table_mania_err) => {
                    let Some(sig) = table_mania_err.signature() else {
                        return Err(AccountClonerError::CommittorServiceError(
                            format!("{:?}", table_mania_err),
                        ));
                    };
                    let cus =
                        changeset_committor.get_transaction_cus(&sig).await;
                    let logs =
                        changeset_committor.get_transaction_logs(&sig).await;
                    let cus_str = cus
                        .map(|cus| format!("{:?}", cus))
                        .unwrap_or("N/A".to_string());
                    let logs_str = logs
                        .map(|logs| format!("{:#?}", logs))
                        .unwrap_or("N/A".to_string());
                    Err(AccountClonerError::CommittorServiceError(format!(
                        "{:?}\nCUs: {cus_str}\nLogs: {logs_str}",
                        table_mania_err
                    )))
                }
                _ => Err(AccountClonerError::CommittorServiceError(format!(
                    "{:?}",
                    err
                ))),
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct AccountClonerPermissions {
    pub allow_cloning_refresh: bool,
    pub allow_cloning_feepayer_accounts: bool,
    pub allow_cloning_undelegated_accounts: bool,
    pub allow_cloning_delegated_accounts: bool,
    pub allow_cloning_program_accounts: bool,
}

impl AccountClonerPermissions {
    pub fn can_clone(&self) -> bool {
        self.allow_cloning_feepayer_accounts
            || self.allow_cloning_undelegated_accounts
            || self.allow_cloning_delegated_accounts
            || self.allow_cloning_program_accounts
    }
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

pub fn standard_blacklisted_accounts(
    validator_id: &Pubkey,
    faucet_id: &Pubkey,
) -> HashSet<Pubkey> {
    // This is buried in the accounts_db::native_mint module and we don't
    // want to take a dependency on that crate just for this ID which won't change
    const NATIVE_SOL_ID: Pubkey =
        solana_sdk::pubkey!("So11111111111111111111111111111111111111112");

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
    blacklisted_accounts.insert(NATIVE_SOL_ID);
    blacklisted_accounts.insert(magic_program::ID);
    blacklisted_accounts.insert(magic_program::MAGIC_CONTEXT_PUBKEY);
    blacklisted_accounts.insert(*validator_id);
    blacklisted_accounts.insert(*faucet_id);
    blacklisted_accounts
}
