use std::{collections::HashSet, sync::Arc, time::Duration};

use conjunto_transwise::{
    transaction_accounts_extractor::TransactionAccountsExtractor,
    transaction_accounts_holder::TransactionAccountsHolder,
    transaction_accounts_snapshot::TransactionAccountsSnapshot,
    transaction_accounts_validator::TransactionAccountsValidator,
    AccountChainState,
};
use futures_util::future::{try_join, try_join_all};
use lazy_static::lazy_static;
use log::*;
use sleipnir_account_fetcher::AccountFetcher;
use sleipnir_account_updates::AccountUpdates;
use sleipnir_bank::bank::Bank;
use sleipnir_program::{
    process_accounts_pending_removal_transaction,
    sleipnir_instruction::AccountModification, traits::AccountsRemover,
};
use sleipnir_transaction_status::TransactionStatusSender;
use solana_sdk::{
    pubkey::Pubkey, signature::Signature, sysvar,
    transaction::SanitizedTransaction,
};

use crate::{
    errors::{AccountsError, AccountsResult},
    execute_legacy_transaction,
    external_accounts::{ExternalReadonlyAccounts, ExternalWritableAccounts},
    traits::{AccountCloner, AccountCommitter, InternalAccountProvider},
    utils::get_epoch,
    AccountCommittee, CommitAccountsPayload, LifecycleMode,
    PendingCommitTransaction, ScheduledCommitsProcessor,
    SendableCommitAccountsPayload, UndelegationRequest,
};

lazy_static! {
    // TODO(vbrunet) - we will need a more general solution to those unfetchable accounts
    // progress tracked here: https://github.com/magicblock-labs/magicblock-validator/issues/124
    static ref BLACKLISTED_ACCOUNTS: HashSet<Pubkey> = {
        let mut accounts = HashSet::new();
        accounts.insert(sysvar::clock::ID);
        accounts.insert(sysvar::epoch_rewards::ID);
        accounts.insert(sysvar::epoch_schedule::ID);
        accounts.insert(sysvar::fees::ID);
        accounts.insert(sysvar::instructions::ID);
        accounts.insert(sysvar::last_restart_slot::ID);
        accounts.insert(sysvar::recent_blockhashes::ID);
        accounts.insert(sysvar::rent::ID);
        accounts.insert(sysvar::rewards::ID);
        accounts.insert(sysvar::slot_hashes::ID);
        accounts.insert(sysvar::slot_history::ID);
        accounts.insert(sysvar::stake_history::ID);
        accounts
    };
}

#[derive(Debug)]
pub struct ExternalAccountsManager<IAP, AFE, ACL, ACM, ARE, AUP, TAE, TAV, SCP>
where
    IAP: InternalAccountProvider,
    AFE: AccountFetcher,
    ACL: AccountCloner,
    ACM: AccountCommitter,
    ARE: AccountsRemover,
    AUP: AccountUpdates,
    TAE: TransactionAccountsExtractor,
    TAV: TransactionAccountsValidator,
    SCP: ScheduledCommitsProcessor,
{
    pub internal_account_provider: IAP,
    pub account_fetcher: AFE,
    pub account_cloner: ACL,
    pub accounts_remover: ARE,
    pub account_committer: Arc<ACM>,
    pub account_updates: AUP,
    pub transaction_accounts_extractor: TAE,
    pub transaction_accounts_validator: TAV,
    pub transaction_status_sender: Option<TransactionStatusSender>,
    pub scheduled_commits_processor: SCP,
    pub external_readonly_accounts: ExternalReadonlyAccounts,
    pub external_writable_accounts: ExternalWritableAccounts,
    pub lifecycle: LifecycleMode,
    pub payer_init_lamports: Option<u64>,
    pub validator_id: Pubkey,
}

impl<IAP, AFE, ACL, ACM, ARE, AUP, TAE, TAV, SCP>
    ExternalAccountsManager<IAP, AFE, ACL, ACM, ARE, AUP, TAE, TAV, SCP>
where
    IAP: InternalAccountProvider,
    AFE: AccountFetcher,
    ACL: AccountCloner,
    ACM: AccountCommitter,
    ARE: AccountsRemover,
    AUP: AccountUpdates,
    TAE: TransactionAccountsExtractor,
    TAV: TransactionAccountsValidator,
    SCP: ScheduledCommitsProcessor,
{
    pub async fn ensure_accounts(
        &self,
        tx: &SanitizedTransaction,
    ) -> AccountsResult<Vec<Signature>> {
        // If this validator does not clone any accounts then we're done
        if self.lifecycle.is_clone_readable_none()
            && self.lifecycle.is_clone_writable_none()
        {
            return Ok(vec![]);
        }

        // 1. Extract all acounts from the transaction
        let accounts_holder = self
            .transaction_accounts_extractor
            .try_accounts_from_sanitized_transaction(tx)?;

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
        signature: String,
    ) -> AccountsResult<Vec<Signature>> {
        let payer_id = accounts_holder.payer;

        // Ensure that we are watching all readonly accounts
        accounts_holder
            .readonly
            .iter()
            .try_for_each(|pubkey| {
                // TODO(vbrunet)
                //  - https://github.com/magicblock-labs/magicblock-validator/issues/95
                //  - handle the case of the payer better, we may not want to track lamport changes
                self.account_updates.ensure_account_monitoring(pubkey)
            })
            .map_err(AccountsError::AccountUpdatesError)?;

        // 2.A Collect all readonly accounts we've never seen before and need to clone as readonly
        let unseen_readonly_ids = if self.lifecycle.is_clone_readable_none() {
            vec![]
        } else {
            accounts_holder
                .readonly
                .into_iter()
                // We never want to clone the validator authority account
                .filter(|pubkey| !self.validator_id.eq(pubkey))
                // We also never fetch some black-listed accounts (sysvars for example)
                .filter(|pubkey| !BLACKLISTED_ACCOUNTS.contains(pubkey))
                // Otherwise check if we know about the account from previous transactions
                .filter(|pubkey| {
                    // If an account has already been cloned and prepared to be used as writable,
                    // it can also be used as readonly, no questions asked, as it is already delegated
                    if self.external_writable_accounts.has(pubkey) {
                        return false;
                    }
                    // If there was an on-chain update since last clone, always re-clone
                    if let Some(cloned_at_slot) = self
                        .external_readonly_accounts
                        .get_cloned_at_slot(pubkey)
                    {
                        if let Some(last_known_update_slot) = self
                            .account_updates
                            .get_last_known_update_slot(pubkey)
                        {
                            if cloned_at_slot < last_known_update_slot {
                                self.external_readonly_accounts.remove(pubkey);
                                return true;
                            }
                        }
                    }
                    // If we don't know of any recent update, and it's still in the cache, it can be used safely
                    if self.external_readonly_accounts.has(pubkey) {
                        return false;
                    }
                    // If somehow the account is already in the validator data for other reason, no need to re-clone it
                    if self.internal_account_provider.has_account(pubkey) {
                        return false;
                    }
                    // If we have no knownledge of the account, clone it
                    true
                })
                .collect::<Vec<_>>()
        };
        trace!("Newly seen readonly pubkeys: {:?}", unseen_readonly_ids);

        // 2.B If needed, Collect all writable accounts we've never seen before and need to clone and prepare as writable
        let unseen_writable_ids = if self.lifecycle.is_clone_writable_none() {
            vec![]
        } else {
            accounts_holder
                .writable
                .into_iter()
                // We never want to clone the validator authority account
                .filter(|pubkey| !self.validator_id.eq(pubkey))
                // If an account has already been cloned and prepared to be used as writable, no need to re-do it
                .filter(|pubkey| !self.external_writable_accounts.has(pubkey))
                // Even if the account is already present in the validator,
                // we still need to prepare it so it can be used as a writable.
                // Because it may only be able to be used as a readonly until modified.
                .collect::<Vec<_>>()
        };
        trace!("Newly seen writable pubkeys: {:?}", unseen_writable_ids);

        // 3.A Fetch the accounts that we've seen for the first time
        let (readonly_snapshot, writable_snapshot) = try_join(
            try_join_all(unseen_readonly_ids.iter().map(|pubkey| {
                self.account_fetcher.fetch_account_chain_snapshot(pubkey)
            })),
            try_join_all(unseen_writable_ids.iter().map(|pubkey| {
                self.account_fetcher.fetch_account_chain_snapshot(pubkey)
            })),
        )
        .await
        .map_err(AccountsError::AccountFetcherError)?;

        // 3.B Validate the accounts that we see for the very first time
        let tx_snapshot = TransactionAccountsSnapshot {
            readonly: readonly_snapshot,
            writable: writable_snapshot,
            payer: accounts_holder.payer,
        };
        if self.lifecycle.requires_ephemeral_validation() {
            self.transaction_accounts_validator
                .validate_ephemeral_transaction_accounts(&tx_snapshot)?;
        }

        // 4.A If a readonly account is not a program, but we only should clone programs then
        //     we have a problem since the account does not exist nor will it be created.
        //     Here we just remove it from the accounts to be cloned and let the  trigger
        //     transaction fail due to the missing account as it normally would.
        //     We have a similar problem if the account was not found at all in which case
        //     it's `is_program` field is `None`.
        let programs_only = self.lifecycle.is_clone_readable_programs_only();

        let cloned_readonly_accounts = tx_snapshot
            .readonly
            .into_iter()
            .filter(|acc| match acc.chain_state.account() {
                // If it exists: Allow the account if its a program or if we allow non-programs to be cloned
                Some(account) => account.executable || !programs_only,
                // Otherwise, don't clone it
                None => false,
            })
            .collect::<Vec<_>>();

        // 4.B We will want to make sure that all accounts that exist on chain and are writable have been cloned
        let cloned_writable_accounts = tx_snapshot
            .writable
            .into_iter()
            .filter(|acc| acc.chain_state.account().is_some())
            .collect::<Vec<_>>();

        // Useful logging of involved writable/readables pubkeys
        if log::log_enabled!(log::Level::Debug) {
            if !cloned_readonly_accounts.is_empty() {
                debug!(
                    "Transaction '{}' triggered readonly account clones: {:?}",
                    signature,
                    cloned_readonly_accounts
                        .iter()
                        .map(|acc| acc.pubkey)
                        .collect::<Vec<_>>(),
                );
            }
            if !cloned_writable_accounts.is_empty() {
                let cloned_writable_descriptions = cloned_writable_accounts
                    .iter()
                    .map(|x| {
                        format!(
                            "{}{}{}",
                            if x.pubkey == payer_id { "[payer]:" } else { "" },
                            x.pubkey,
                            match x.chain_state {
                                AccountChainState::NewAccount => "NewAccount",
                                AccountChainState::Undelegated { .. } =>
                                    "Undelegated",
                                AccountChainState::Delegated { .. } =>
                                    "Delegated",
                                AccountChainState::Inconsistent { .. } =>
                                    "Inconsistent",
                            },
                        )
                    })
                    .collect::<Vec<_>>();
                debug!(
                    "Transaction '{}' triggered writable account clones: {:?}",
                    signature, cloned_writable_descriptions
                );
            }
        }

        let mut signatures = vec![];

        // 5.A Clone the unseen readonly accounts without any modifications
        for cloned_readonly_account in cloned_readonly_accounts {
            let mut clone_signatures = self
                .account_cloner
                .clone_account(
                    &cloned_readonly_account.pubkey,
                    cloned_readonly_account.chain_state.account(),
                    None,
                )
                .await?;
            signatures.append(&mut clone_signatures);
            self.external_readonly_accounts.insert(
                cloned_readonly_account.pubkey,
                cloned_readonly_account.at_slot,
            );
        }

        // 5.B Clone the unseen writable accounts and apply modifications so they represent
        //     the undelegated state they would have on chain, i.e. with the original owner
        for cloned_writable_account in cloned_writable_accounts {
            // Create and the transaction to dump data array, lamports and owner change to the local state
            let mut overrides = cloned_writable_account
                .chain_state
                .delegation_record()
                .as_ref()
                .map(|x| AccountModification {
                    owner: Some(x.owner),
                    ..Default::default()
                });
            if cloned_writable_account.pubkey == payer_id {
                if let Some(lamports) = self.payer_init_lamports {
                    match overrides {
                        Some(ref mut x) => x.lamports = Some(lamports),
                        None => {
                            overrides = Some(AccountModification {
                                lamports: Some(lamports),
                                ..Default::default()
                            })
                        }
                    }
                }
            }
            let mut clone_signatures = self
                .account_cloner
                .clone_account(
                    &cloned_writable_account.pubkey,
                    cloned_writable_account.chain_state.account(),
                    overrides,
                )
                .await?;
            signatures.append(&mut clone_signatures);
            // Remove the account from the readonlys and add it to writables
            self.external_readonly_accounts
                .remove(&cloned_writable_account.pubkey);
            self.external_writable_accounts.insert(
                cloned_writable_account.pubkey,
                cloned_writable_account.at_slot,
                cloned_writable_account
                    .chain_state
                    .delegation_record()
                    .as_ref()
                    .map(|x| x.commit_frequency),
            );
        }

        if log::log_enabled!(log::Level::Debug) && !signatures.is_empty() {
            debug!("Transactions {:?}", signatures,);
        }

        Ok(signatures)
    }

    /// This will look at the time that passed since the last commit and determine
    /// which accounts are due to be committed, perform that step for them
    /// and return the signatures of the transactions that were sent to the cluster.
    pub async fn commit_delegated(&self) -> AccountsResult<Vec<Signature>> {
        let now = get_epoch();
        // Find all accounts that are due to be committed let accounts_to_be_committed = self
        let accounts_to_be_committed = self
            .external_writable_accounts
            .read_accounts()
            .values()
            .filter(|x| x.needs_commit(now))
            .map(|x| x.pubkey)
            .collect::<Vec<_>>();

        // if accounts_to_be_committed.is_empty() {
        //     return Ok(vec![]);
        // }

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
            if let Some(acc) =
                self.external_writable_accounts.read_accounts().get(&pubkey)
            {
                acc.mark_as_committed(now);
            } else {
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
        self.external_writable_accounts
            .read_accounts()
            .get(pubkey)
            .map(|x| x.last_committed_at())
    }

    pub async fn process_scheduled_commits(&self) -> AccountsResult<()> {
        self.scheduled_commits_processor
            .process(
                &self.account_committer,
                &self.internal_account_provider,
                &self.accounts_remover,
            )
            .await
    }

    // -----------------
    // Accounts removal
    // -----------------
    pub fn remove_accounts_pending_removal(
        &self,
        bank: &Arc<Bank>,
    ) -> AccountsResult<Option<Signature>> {
        let accounts = self.accounts_remover.accounts_pending_removal();
        if !accounts.is_empty() {
            // We first clear the accounts from the accounts manager in order to
            // force a fresh clone should anyone want to use the account
            for pubkey in &accounts {
                self.external_readonly_accounts.remove(pubkey);
                self.external_writable_accounts.remove(pubkey);
            }

            // Then we remove the accounts from the bank via a transaction
            let blockhash = bank.last_blockhash();
            let tx = process_accounts_pending_removal_transaction(
                accounts, blockhash,
            );
            let sig = execute_legacy_transaction(
                tx,
                bank,
                self.transaction_status_sender.as_ref(),
            )?;

            Ok(Some(sig))
        } else {
            Ok(None)
        }
    }
}
