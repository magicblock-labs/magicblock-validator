use std::collections::HashSet;

use async_trait::async_trait;
use magicblock_metrics::metrics::HistogramTimer;
use magicblock_program::magic_scheduled_base_intent::CommittedAccountV2;
use solana_rpc_client::rpc_client::SerializableTransaction;
use solana_sdk::{
    account::{Account, AccountSharedData, ReadableAccount},
    pubkey::Pubkey,
    signature::Signature,
    transaction::Transaction,
};

use crate::errors::AccountsResult;

#[async_trait]
pub trait ScheduledCommitsProcessor: Send + Sync + 'static {
    /// Processes all commits that were scheduled and accepted
    async fn process(&self) -> AccountsResult<()>;

    /// Returns the number of commits that were scheduled and accepted
    fn scheduled_commits_len(&self) -> usize;

    /// Clears all scheduled commits
    fn clear_scheduled_commits(&self);

    /// Stop processor
    fn stop(&self);
}

// TODO(edwin): remove this
#[derive(Clone)]
pub struct AccountCommittee {
    /// The pubkey of the account to be committed.
    pub pubkey: Pubkey,
    /// The pubkey of the owner of the account to be committed.
    pub owner: Pubkey,
    /// The current account state.
    /// NOTE: if undelegation was requested the owner is set to the
    /// delegation program when accounts are committed.
    pub account_data: AccountSharedData,
    /// Slot at which the commit was scheduled.
    pub slot: u64,
    /// Only present if undelegation was requested.
    pub undelegation_requested: bool,
}

impl From<AccountCommittee> for CommittedAccountV2 {
    fn from(value: AccountCommittee) -> Self {
        CommittedAccountV2 {
            pubkey: value.pubkey,
            account: Account {
                lamports: value.account_data.lamports(),
                data: value.account_data.data().to_vec(),
                // TODO(edwin): shall take from account_data instead?
                owner: value.owner,
                executable: value.account_data.executable(),
                rent_epoch: value.account_data.rent_epoch(),
            },
        }
    }
}

#[derive(Debug)]
pub struct CommitAccountsTransaction {
    /// The transaction that is running on chain to commit and possibly undelegate
    /// accounts.
    pub transaction: Transaction,
    /// Accounts that are undelegated as part of the transaction.
    pub undelegated_accounts: HashSet<Pubkey>,
    /// Accounts that are only committed and not undelegated as part of the transaction.
    pub committed_only_accounts: HashSet<Pubkey>,
}

impl CommitAccountsTransaction {
    pub fn get_signature(&self) -> Signature {
        *self.transaction.get_signature()
    }
}

#[derive(Debug)]
pub struct CommitAccountsPayload {
    /// The transaction that commits the accounts.
    /// None if no accounts need to be committed.
    pub transaction: Option<CommitAccountsTransaction>,
    /// The pubkeys and data of the accounts that were committed.
    pub committees: Vec<(Pubkey, AccountSharedData)>,
}

/// Same as [CommitAccountsPayload] but one that is actionable
#[derive(Debug)]
pub struct SendableCommitAccountsPayload {
    pub transaction: CommitAccountsTransaction,
    /// The pubkeys and data of the accounts that were committed.
    pub committees: Vec<(Pubkey, AccountSharedData)>,
}

impl SendableCommitAccountsPayload {
    pub fn get_signature(&self) -> Signature {
        self.transaction.get_signature()
    }
}

/// Represents a transaction that has been sent to chain and is pending
/// completion.
#[derive(Debug)]
pub struct PendingCommitTransaction {
    /// The signature of the transaction that was sent to chain.
    pub signature: Signature,
    /// The accounts that are undelegated on chain as part of this transaction.
    pub undelegated_accounts: HashSet<Pubkey>,
    /// Accounts that are only committed and not undelegated as part of the transaction.
    pub committed_only_accounts: HashSet<Pubkey>,
    /// Timer that is started when we send the commit to chain and ends when
    /// the transaction is confirmed.
    pub timer: HistogramTimer,
}
