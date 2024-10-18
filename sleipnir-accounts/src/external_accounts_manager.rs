use std::{
    collections::{hash_map::Entry, HashMap},
    sync::{Arc, RwLock},
    time::Duration,
    vec,
};

use conjunto_transwise::{
    transaction_accounts_extractor::TransactionAccountsExtractor,
    transaction_accounts_holder::TransactionAccountsHolder,
    transaction_accounts_snapshot::TransactionAccountsSnapshot,
    transaction_accounts_validator::TransactionAccountsValidator,
    AccountChainSnapshotShared, AccountChainState, CommitFrequency,
};
use futures_util::future::{try_join, try_join_all};
use log::*;
use sleipnir_account_cloner::{AccountCloner, AccountClonerOutput};
use sleipnir_accounts_api::InternalAccountProvider;
use sleipnir_core::magic_program;
use solana_sdk::{
    pubkey::Pubkey, signature::Signature, transaction::SanitizedTransaction,
};

use crate::{
    errors::{AccountsError, AccountsResult},
    traits::{AccountCommitter, UndelegationRequest},
    utils::get_epoch,
    AccountCommittee, CommitAccountsPayload, LifecycleMode,
    PendingCommitTransaction, ScheduledCommitsProcessor,
    SendableCommitAccountsPayload,
};

#[derive(Debug)]
pub struct ExternalCommitableAccount {
    pubkey: Pubkey,
    commit_frequency: Duration,
    last_commit_at: Duration,
}

impl ExternalCommitableAccount {
    pub fn new(
        pubkey: &Pubkey,
        commit_frequency: &CommitFrequency,
        now: &Duration,
    ) -> Self {
        let commit_frequency = Duration::from(*commit_frequency);
        // We don't want to commit immediately after cloning, thus we consider
        // the account as committed at clone time until it is updated after
        // a commit
        let last_commit_at = *now;
        Self {
            pubkey: *pubkey,
            commit_frequency,
            last_commit_at,
        }
    }
    pub fn needs_commit(&self, now: &Duration) -> bool {
        *now > self.last_commit_at + self.commit_frequency
    }
    pub fn last_committed_at(&self) -> Duration {
        self.last_commit_at
    }
    pub fn mark_as_committed(&mut self, now: &Duration) {
        self.last_commit_at = *now
    }
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
        // Extract all acounts from the transaction
        let accounts_holder = self
            .transaction_accounts_extractor
            .try_accounts_from_sanitized_transaction(tx)
            .map_err(Box::new)?;
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
        let (readonly_clone_outputs, writable_clone_outputs) = try_join(
            try_join_all(
                accounts_holder
                    .readonly
                    .into_iter()
                    .filter(should_clone_account)
                    .map(|pubkey| self.account_cloner.clone_account(&pubkey)),
            ),
            try_join_all(
                accounts_holder
                    .writable
                    .into_iter()
                    .filter(should_clone_account)
                    .map(|pubkey| self.account_cloner.clone_account(&pubkey)),
            ),
        )
        .await
        .map_err(AccountsError::AccountClonerError)?;

        // Commitable account scheduling initialization
        for readonly_clone_output in readonly_clone_outputs.iter() {
            self.start_commit_frequency_counters_if_needed(
                readonly_clone_output,
            );
        }
        for writable_clone_output in writable_clone_outputs.iter() {
            self.start_commit_frequency_counters_if_needed(
                writable_clone_output,
            );
        }

        // Collect all the signatures involved in the cloning
        let signatures: Vec<Signature> = readonly_clone_outputs
            .iter()
            .chain(writable_clone_outputs.iter())
            .filter_map(|clone_output| match clone_output {
                AccountClonerOutput::Cloned { signature, .. } => {
                    Some(*signature)
                }
                AccountClonerOutput::Unclonable { .. } => None,
            })
            .collect();

        // Validate that the accounts involved in the transaction are valid for an ephemeral
        if self.lifecycle.requires_ephemeral_validation() {
            // For now we'll allow readonly accounts to be not properly clonable but still usable in a transaction
            let readonly_snapshots = readonly_clone_outputs
                .into_iter()
                .filter_map(|clone_output| match clone_output {
                    AccountClonerOutput::Cloned {
                        account_chain_snapshot,
                        ..
                    } => Some(account_chain_snapshot),
                    AccountClonerOutput::Unclonable { .. } => None,
                })
                .collect::<Vec<AccountChainSnapshotShared>>();
            // Ephemeral will only work if all writable accounts involved in a transaction are properly cloned
            let writable_snapshots = writable_clone_outputs.into_iter()
                .map(|clone_output| match clone_output {
                    AccountClonerOutput::Cloned{account_chain_snapshot, ..} => Ok(account_chain_snapshot),
                    AccountClonerOutput::Unclonable{ pubkey, reason, ..} => {
                        Err(AccountsError::UnclonableAccountUsedAsWritableInEphemeral(pubkey, reason))
                    }
                })
                .collect::<AccountsResult<Vec<AccountChainSnapshotShared>>>()?;
            // Run the validation specific to the ephemeral
            self.transaction_accounts_validator
                .validate_ephemeral_transaction_accounts(
                    &TransactionAccountsSnapshot {
                        readonly: readonly_snapshots,
                        writable: writable_snapshots,
                        payer: accounts_holder.payer,
                    },
                )
                .map_err(Box::new)?;
        }

        // Done
        Ok(signatures)
    }

    fn start_commit_frequency_counters_if_needed(
        &self,
        clone_output: &AccountClonerOutput,
    ) {
        if let AccountClonerOutput::Cloned {
            account_chain_snapshot,
            ..
        } = clone_output
        {
            if let AccountChainState::Delegated {
                delegation_record, ..
            } = &account_chain_snapshot.chain_state
            {
                match self.external_commitable_accounts
                    .write()
                    .expect(
                    "RwLock of ExternalAccountsManager.external_commitable_accounts is poisoned",
                    )
                    .entry(account_chain_snapshot.pubkey)
                {
                    Entry::Occupied(mut _entry) => {},
                    Entry::Vacant(entry) => {
                        entry.insert(ExternalCommitableAccount::new(&account_chain_snapshot.pubkey, &delegation_record.commit_frequency, &get_epoch()));
                    },
                }
            }
        };
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
            .filter(|x| x.needs_commit(&now))
            .map(|x| x.get_pubkey())
            .collect::<Vec<_>>();
        if accounts_to_be_committed.is_empty() {
            return Ok(vec![]);
        }

        // NOTE: the scheduled commits use the slot at which the commit was scheduled
        // However frequent commits run async and could be running before a slot is completed
        // Thus they really commit in between two slots instead of at the end of a particular slot.
        // Therefore we use the current slot which could result in two commits with the same
        // slot. However since we most likely will phase out frequent commits we accept this
        // inconsistency for now.
        let slot = self.internal_account_provider.get_slot();
        let commit_infos = self
            .create_transactions_to_commit_specific_accounts(
                accounts_to_be_committed,
                slot,
                None,
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
        // NOTE: we ignore the [PendingCommitTransaction::undelegated_accounts] here since for
        // scheduled commits we never request undelegation
        self.run_transactions_to_commit_specific_accounts(now, sendables)
            .await
            .map(|pendings| pendings.into_iter().map(|x| x.signature).collect())
    }

    async fn create_transactions_to_commit_specific_accounts(
        &self,
        accounts_to_be_committed: Vec<Pubkey>,
        slot: u64,
        undelegation_request: Option<UndelegationRequest>,
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
                    slot,
                    undelegation_request: undelegation_request.clone(),
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
        let txs = try_join_all(committees.into_iter().map(|commitee| {
            self.account_committer
                .create_commit_accounts_transaction(vec![commitee])
        }))
        .await?;

        Ok(txs)
    }

    pub async fn run_transactions_to_commit_specific_accounts(
        &self,
        now: Duration,
        payloads: Vec<SendableCommitAccountsPayload>,
    ) -> AccountsResult<Vec<PendingCommitTransaction>> {
        let pubkeys = payloads
            .iter()
            .flat_map(|x| x.committees.iter().map(|x| x.0))
            .collect::<Vec<_>>();

        // Commit all transactions
        let pending_commits = self
            .account_committer
            .send_commit_transactions(payloads)
            .await?;

        // Mark committed accounts
        for pubkey in pubkeys {
            if let Some(acc) = self
                .external_commitable_accounts
                .write()
                .expect(
                "RwLock of ExternalAccountsManager.external_commitable_accounts is poisoned",
                )
                .get_mut(&pubkey)
            {
                acc.mark_as_committed(&now);
            }
            else {
                // This should never happen
                error!(
                    "Account '{}' disappeared while being committed",
                    pubkey
                );
            }
        }

        Ok(pending_commits)
    }

    pub fn last_commit(&self, pubkey: &Pubkey) -> Option<Duration> {
        self.external_commitable_accounts
            .read()
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

fn should_clone_account(pubkey: &Pubkey) -> bool {
    pubkey != &magic_program::MAGIC_CONTEXT_PUBKEY
}
