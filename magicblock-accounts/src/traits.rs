use std::{collections::HashSet, sync::Arc};

use async_trait::async_trait;
use magicblock_accounts_api::InternalAccountProvider;
use magicblock_committor_service::L1MessageCommittor;
use magicblock_metrics::metrics::HistogramTimer;
use magicblock_program::magic_scheduled_l1_message::{
    CommittedAccountV2, ScheduledL1Message,
};
use solana_rpc_client::rpc_client::SerializableTransaction;
use solana_sdk::{
    account::{Account, AccountSharedData, ReadableAccount},
    clock::Epoch,
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

#[async_trait]
pub trait AccountCommitter: Send + Sync + 'static {
    /// Creates a transaction to commit each provided account unless it determines
    /// that it isn't necessary, i.e. when the previously committed state is the same
    /// as the [commit_state_data].
    /// Returns the transaction committing the accounts and the pubkeys of accounts
    /// it did commit
    async fn create_commit_accounts_transaction(
        &self,
        committees: Vec<AccountCommittee>,
    ) -> AccountsResult<CommitAccountsPayload>;

    /// Returns the main-chain signatures of the commit transactions
    /// This will only fail due to network issues, not if the transaction failed.
    /// Therefore we want to either fail all transactions or none which is why
    /// we return a `Result<Vec>` instead of a `Vec<Result>`.
    async fn send_commit_transactions(
        &self,
        payloads: Vec<SendableCommitAccountsPayload>,
    ) -> AccountsResult<Vec<PendingCommitTransaction>>;

    /// Confirms all transactions for the given [pending_commits] with 'confirmed'
    /// commitment level.
    /// Updates the metrics for each transaction in order to record the time it took
    /// to fully confirm it on chain.
    async fn confirm_pending_commits(
        &self,
        pending_commits: Vec<PendingCommitTransaction>,
    );
}
