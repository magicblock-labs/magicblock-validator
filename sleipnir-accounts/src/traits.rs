use std::sync::Arc;

use async_trait::async_trait;
use sleipnir_accounts_api::InternalAccountProvider;
use solana_sdk::{
    account::AccountSharedData, pubkey::Pubkey, signature::Signature,
    transaction::Transaction,
};

use crate::errors::AccountsResult;

#[async_trait]
pub trait ScheduledCommitsProcessor {
    async fn process<
        AC: AccountCommitter,
        IAP: InternalAccountProvider,
        AR: AccountsRemover,
    >(
        &self,
        committer: &Arc<AC>,
        account_provider: &IAP,
        accounts_remover: &AR,
    ) -> AccountsResult<()>;
}

pub struct AccountCommittee {
    /// The pubkey of the account to be committed.
    pub pubkey: Pubkey,
    /// The current account state.
    /// NOTE: if undelegation was requested the owner is set to the
    /// delegation program when accounts are committed.
    pub account_data: AccountSharedData,
    /// Slot at which the commit was scheduled.
    pub slot: u64,
    /// Only present if undelegation was requested.
    pub undelegation_request: Option<UndelegationRequest>,
}
pub struct CommitAccountsTransaction {
    /// The transaction that is running on chain to commit and possibly undelegate
    /// accounts.
    pub transaction: Transaction,
    /// Accounts that are undelegated as part of the transaction.
    /// They need to be removed from our validator when the transaction completes.
    pub undelegated_accounts: Vec<Pubkey>,
}

impl CommitAccountsTransaction {
    pub fn get_signature(&self) -> Signature {
        *self.transaction.get_signature()
    }
}

pub struct CommitAccountsPayload {
    /// The transaction that commits the accounts.
    /// None if no accounts need to be committed.
    pub transaction: Option<CommitAccountsTransaction>,
    /// The pubkeys and data of the accounts that were committed.
    pub committees: Vec<(Pubkey, AccountSharedData)>,
}

/// Same as [CommitAccountsPayload] but one that is actionable
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
    /// They need to be removed from our validator when the transaction completes.
    pub undelegated_accounts: Vec<Pubkey>,
}

#[async_trait]
pub trait AccountCommitter: Send + Sync + 'static {
    /// Creates a transaction to commit each provided account unless it determines
    /// that it isn't necessary, i.e. when the previously committed state is the same
    /// as the [commit_state_data].
    /// Returns the transaction committing the accounts and the pubkeys of accounts
    /// it did commit
    async fn create_commit_accounts_transactions(
        &self,
        committees: Vec<AccountCommittee>,
    ) -> AccountsResult<Vec<CommitAccountsPayload>>;

    /// Returns the main-chain signatures of the commit transactions
    /// This will only fail due to network issues, not if the transaction failed.
    /// Therefore we want to either fail all transactions or none which is why
    /// we return a `Result<Vec>` instead of a `Vec<Result>`.
    async fn send_commit_transactions(
        &self,
        payloads: Vec<SendableCommitAccountsPayload>,
    ) -> AccountsResult<Vec<PendingCommitTransaction>>;
}
