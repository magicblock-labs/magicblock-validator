use std::{
    collections::{hash_map::Entry, HashMap},
    sync::{Arc, RwLock},
    time::Duration,
};

use conjunto_transwise::{
    transaction_accounts_extractor::TransactionAccountsExtractor,
    transaction_accounts_holder::TransactionAccountsHolder,
    transaction_accounts_snapshot::TransactionAccountsSnapshot,
    transaction_accounts_validator::TransactionAccountsValidator,
    AccountChainState, CommitFrequency,
};
use futures_util::future::{try_join, try_join_all};
use log::*;
use sleipnir_account_cloner::AccountCloner;
use solana_sdk::{
    pubkey::Pubkey, signature::Signature, transaction::SanitizedTransaction,
};

use crate::{
    errors::{AccountsError, AccountsResult},
    traits::{AccountCommitter, InternalAccountProvider},
    utils::get_epoch,
    AccountCommittee, CommitAccountsPayload, LifecycleMode,
    ScheduledCommitsProcessor, SendableCommitAccountsPayload,
};

#[derive(Debug)]
pub struct ExternalCommitableAccount {
    pubkey: Pubkey,
}

impl ExternalCommitableAccount {
    pub fn new(pubkey: &Pubkey, commit_frequency: &CommitFrequency) -> Self {
        Self { pubkey: *pubkey }
    }

    pub fn needs_commit(&self, now: &Duration) -> bool {
        false
    }

    pub fn last_committed_at(&self) -> Duration {

    }

    pub fn mark_as_committed(&mut self, now: &Duration) {}

    pub fn get_pubkey(&self) -> Pubkey {
        self.pubkey
    }
}

#[derive(Debug)]
pub struct ExternalAccountsManager<IAP, ACL, ACM, TAE, TAV, SCP>
where
    IAP: InternalAccountProvider,
    ACL: AccountCloner,
    ACM: AccountCommitter,
    TAE: TransactionAccountsExtractor,
    TAV: TransactionAccountsValidator,
    SCP: ScheduledCommitsProcessor,
{
    pub internal_account_provider: IAP,
    pub account_cloner: ACL,
    pub account_committer: Arc<ACM>,
    pub transaction_accounts_extractor: TAE,
    pub transaction_accounts_validator: TAV,
    pub scheduled_commits_processor: SCP,
    pub lifecycle: LifecycleMode,
    pub external_commitable_accounts:
        RwLock<HashMap<Pubkey, ExternalCommitableAccount>>,
}

impl<IAP, ACL, ACM, TAE, TAV, SCP>
    ExternalAccountsManager<IAP, ACL, ACM, TAE, TAV, SCP>
where
    IAP: InternalAccountProvider,
    ACL: AccountCloner,
    ACM: AccountCommitter,
    TAE: TransactionAccountsExtractor,
    TAV: TransactionAccountsValidator,
    SCP: ScheduledCommitsProcessor,
{
    pub async fn ensure_accounts(
        &self,
        tx: &SanitizedTransaction,
    ) -> AccountsResult<Vec<Signature>> {
        // If this validator does not clone any accounts, then we're done
        if self.lifecycle.is_offline() {
            return Ok(vec![]);
        }
        // Extract all acounts from the transaction
        let accounts_holder = self
            .transaction_accounts_extractor
            .try_accounts_from_sanitized_transaction(tx)?;
        // Make sure all accounts used by the transaction are cloned properly if needed
        self.ensure_accounts_from_holder(
            accounts_holder,
            tx.signature().to_string(),
        )
        .await
    }

    // Direct use for tests only
    pub async fn ensure_accounts_from_holder(
        &self,
        accounts_holder: TransactionAccountsHolder,
        _signature: String,
    ) -> AccountsResult<Vec<Signature>> {
        // Clone all the accounts involved in the transaction in parallel
        let (readonly_snapshots, writable_snapshots) =
            try_join(
                try_join_all(
                    accounts_holder.readonly.iter().map(|pubkey| {
                        self.account_cloner.clone_account(pubkey)
                    }),
                ),
                try_join_all(
                    accounts_holder.writable.iter().map(|pubkey| {
                        self.account_cloner.clone_account(pubkey)
                    }),
                ),
            )
            .await
            .map_err(AccountsError::AccountClonerError)?;
        // Validate the accounts involved in the transaction
        let tx_snapshot = TransactionAccountsSnapshot {
            readonly: readonly_snapshots,
            writable: writable_snapshots.clone(),
            payer: accounts_holder.payer,
        };
        if self.lifecycle.requires_ephemeral_validation() {
            self.transaction_accounts_validator
                .validate_ephemeral_transaction_accounts(&tx_snapshot)?;
        }
        // Commitable account scheduling initialization
        for writable_snapshot in writable_snapshots {
            match &writable_snapshot.chain_state {
                AccountChainState::Delegated {
                    delegation_record,
                    ..
                } => 
                    match self.external_commitable_accounts.write()            .expect(
                        "RwLock of ExternalAccountsManager.external_commitable_accounts is poisoned",
                    ).entry(writable_snapshot.pubkey) {
                        Entry::Occupied(mut entry) => {},
                        Entry::Vacant(entry) => {
                            entry.insert(ExternalCommitableAccount::new(&writable_snapshot.pubkey, &delegation_record.commit_frequency));
                        },
                    }
                
                _ => {}
            }
        }
        // Done
        Ok(vec![])
    }

    /// This will look at the time that passed since the last commit and determine
    /// which accounts are due to be committed, perform that step for them
    /// and return the signatures of the transactions that were sent to the cluster.
    pub async fn commit_delegated(&self) -> AccountsResult<Vec<Signature>> {
        let now = get_epoch();
        // Find all accounts that are due to be committed let accounts_to_be_committed = self
        let accounts_to_be_committed = self
            .external_commitable_accounts
            .read()
            .expect(
                "RwLock of ExternalAccountsManager.external_commitable_accounts is poisoned",
            )
            .values()
            .filter(|x| x.needs_commit(now))
            .map(|x| x.get_pubkey())
            .collect::<Vec<_>>();
        let commit_infos = self
            .create_transactions_to_commit_specific_accounts(
                accounts_to_be_committed,
            )
            .await?;
        let sendables = commit_infos
            .into_iter()
            .flat_map(|x| match x.transaction {
                Some(tx) => Some(SendableCommitAccountsPayload {
                    transaction: tx,
                    committees: x.committees,
                }),
                None => None,
            })
            .collect::<Vec<_>>();
        self.run_transactions_to_commit_specific_accounts(now, sendables)
            .await
    }

    pub async fn create_transactions_to_commit_specific_accounts(
        &self,
        accounts_to_be_committed: Vec<Pubkey>,
    ) -> AccountsResult<Vec<CommitAccountsPayload>> {
        // Get current account states from internal account provider
        let mut committees = Vec::new();
        for pubkey in &accounts_to_be_committed {
            let account_state =
                self.internal_account_provider.get_account(pubkey);
            if let Some(acc) = account_state {
                committees.push(AccountCommittee {
                    pubkey: *pubkey,
                    account_data: acc,
                });
            } else {
                error!(
                    "Cannot find state for account that needs to be committed '{}' ",
                    pubkey
                );
            }
        }

        // NOTE: Once we run into issues that the data to be committed in a single
        // transaction is too large, we can split these into multiple batches
        // That is why we return a Vec of CreateCommitAccountsTransactionResult
        let txs = self
            .account_committer
            .create_commit_accounts_transactions(committees)
            .await?;

        Ok(txs)
    }

    pub async fn run_transactions_to_commit_specific_accounts(
        &self,
        now: Duration,
        payloads: Vec<SendableCommitAccountsPayload>,
    ) -> AccountsResult<Vec<Signature>> {
        let pubkeys = payloads
            .iter()
            .flat_map(|x| x.committees.iter().map(|x| x.0))
            .collect::<Vec<_>>();

        // Commit all transactions
        let signatures = self
            .account_committer
            .send_commit_transactions(payloads)
            .await?;

        // Mark committed accounts
        for pubkey in pubkeys {
            if let Some(acc) = self.external_commitable_accounts            .write()
            .expect(
                "RwLock of ExternalAccountsManager.external_commitable_accounts is poisoned",
            )
.get_mut(&pubkey) {
                acc.mark_as_committed(&now);
            } else {
                // This should never happen
                error!(
                    "Account '{}' disappeared while being committed",
                    pubkey
                );
            }
        }

        Ok(signatures)
    }

    pub fn last_commit(&self, pubkey: &Pubkey) -> Option<Duration> {
        self.external_commitable_accounts            .read()
        .expect(
            "RwLock of ExternalAccountsManager.external_commitable_accounts is poisoned",
        )

            .get(pubkey)
            .map(|x| x.last_committed_at())
    }

    pub async fn process_scheduled_commits(&self) -> AccountsResult<()> {
        self.scheduled_commits_processor
            .process(&self.account_committer, &self.internal_account_provider)
            .await
    }
}
