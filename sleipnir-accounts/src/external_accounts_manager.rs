use std::{collections::HashMap, time::Duration};

use conjunto_transwise::{
    transaction_accounts_holder::TransactionAccountsHolder,
    validated_accounts::ValidateAccountsConfig, TransactionAccountsExtractor,
    ValidatedAccountsProvider,
};
use log::*;
use sleipnir_account_updates::AccountUpdates;
use sleipnir_mutator::AccountModification;
use solana_rpc_client::rpc_client::SerializableTransaction;
use solana_sdk::{
    account::AccountSharedData,
    pubkey::Pubkey,
    signature::Signature,
    transaction::{SanitizedTransaction, Transaction},
};

use crate::{
    config::{ExternalReadonlyMode, ExternalWritableMode},
    errors::AccountsResult,
    external_accounts::{ExternalReadonlyAccounts, ExternalWritableAccounts},
    traits::{AccountCloner, AccountCommitter, InternalAccountProvider},
    utils::get_epoch,
};

pub enum CommitAccountInfo {
    /// The account state was committed via the wrapped transaction
    Committed(CommitAccountTransaction),
    /// The account state was not committed since it did not change since the last commit
    NotCommitted,
}
impl CommitAccountInfo {
    pub fn signature(&self) -> Option<Signature> {
        use CommitAccountInfo::*;
        match self {
            Committed(info) => Some(*info.transaction.get_signature()),
            NotCommitted => None,
        }
    }
}

pub struct CommitAccountTransaction {
    pub transaction: Transaction,
    pub commit_state_data: AccountSharedData,
}

#[derive(Debug)]
pub struct ExternalAccountsManager<IAP, ACL, ACM, ACU, VAP, TAE>
where
    IAP: InternalAccountProvider,
    ACL: AccountCloner,
    ACM: AccountCommitter,
    ACU: AccountUpdates,
    VAP: ValidatedAccountsProvider,
    TAE: TransactionAccountsExtractor,
{
    pub internal_account_provider: IAP,
    pub account_cloner: ACL,
    pub account_committer: ACM,
    pub account_updates: ACU,
    pub validated_accounts_provider: VAP,
    pub transaction_accounts_extractor: TAE,
    pub external_readonly_accounts: ExternalReadonlyAccounts,
    pub external_writable_accounts: ExternalWritableAccounts,
    pub external_readonly_mode: ExternalReadonlyMode,
    pub external_writable_mode: ExternalWritableMode,
    pub create_accounts: bool,
    pub payer_init_lamports: Option<u64>,
}

impl<IAP, ACL, ACM, ACU, VAP, TAE>
    ExternalAccountsManager<IAP, ACL, ACM, ACU, VAP, TAE>
where
    IAP: InternalAccountProvider,
    ACL: AccountCloner,
    ACM: AccountCommitter,
    ACU: AccountUpdates,
    VAP: ValidatedAccountsProvider,
    TAE: TransactionAccountsExtractor,
{
    pub async fn ensure_accounts(
        &self,
        tx: &SanitizedTransaction,
    ) -> AccountsResult<Vec<Signature>> {
        // If this validator does not clone any accounts then we're done
        if self.external_readonly_mode.is_clone_none()
            && self.external_writable_mode.is_clone_none()
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
        // 2.A Collect all readonly accounts we've never seen before and need to clone as readonly
        let unseen_readonly_accounts =
            if self.external_readonly_mode.is_clone_none() {
                vec![]
            } else {
                accounts_holder
                    .readonly
                    .into_iter()
                    .filter(|pubkey| {
                        // If an account has already been cloned and prepared to be used as writable,
                        // it can also be used as readonly, no questions asked, as it is already delegated
                        if self.external_writable_accounts.has(pubkey) {
                            return false;
                        }
                        // TODO(vbrunet)
                        //  - https://github.com/magicblock-labs/magicblock-validator/issues/95
                        //  - handle the case of the payer better, we may not want to track lamport changes
                        self.account_updates.request_account_monitoring(pubkey);
                        // If there was an on-chain update since last clone, always re-clone
                        if let Some(cloned_at_slot) = self
                            .external_readonly_accounts
                            .get_cloned_at_slot(pubkey)
                        {
                            if self.account_updates.has_known_update_since_slot(
                                pubkey,
                                cloned_at_slot,
                            ) {
                                self.external_readonly_accounts.remove(pubkey);
                                return true;
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
        trace!(
            "Newly seen readonly accounts: {:?}",
            unseen_readonly_accounts
        );

        // 2.B Collect all writable accounts we've never seen before and need to clone and prepare as writable
        let unseen_writable_accounts = if self
            .external_writable_mode
            .is_clone_none()
        {
            vec![]
        } else {
            accounts_holder
                .writable
                .into_iter()
                // If an account has already been cloned and prepared to be used as writable, no need to re-do it
                .filter(|pubkey| !self.external_writable_accounts.has(pubkey))
                // Even if the account is already present in the validator,
                // we still need to prepare it so it can be used as a writable.
                // Because it may only be able to be used as a readonly until modified.
                .collect::<Vec<_>>()
        };
        trace!(
            "Newly seen writable accounts: {:?}",
            unseen_writable_accounts
        );

        // 3. Validate only the accounts that we see for the very first time
        let validated_accounts = self
            .validated_accounts_provider
            .validate_accounts(
                &TransactionAccountsHolder {
                    readonly: unseen_readonly_accounts,
                    writable: unseen_writable_accounts,
                    payer: accounts_holder.payer,
                },
                &ValidateAccountsConfig {
                    allow_new_accounts: self.create_accounts,
                    // Here we specify if we can clone all writable accounts or
                    // only the ones that were delegated
                    require_delegation: self
                        .external_writable_mode
                        .is_clone_delegated_only(),
                },
            )
            .await?;

        // 4.A If a readonly account is not a program, but we only should clone programs then
        //     we have a problem since the account does not exist nor will it be created.
        //     Here we just remove it from the accounts to be cloned and let the  trigger
        //     transaction fail due to the missing account as it normally would.
        //     We have a similar problem if the account was not found at all in which case
        //     it's `is_program` field is `None`.
        let programs_only =
            self.external_readonly_mode.is_clone_programs_only();

        let cloned_readonly_accounts = validated_accounts
            .readonly
            .into_iter()
            .filter(|acc| match &acc.account {
                // If it exists: Allow the account if its a program or if we allow non-programs to be cloned
                Some(account) => account.executable || !programs_only,
                // Otherwise, don't clone it
                _ => false,
            })
            .collect::<Vec<_>>();

        // 4.B We will want to make sure that all accounts that exist on chain and are writable have been cloned
        let cloned_writable_accounts = validated_accounts
            .writable
            .into_iter()
            .filter(|acc| acc.account.is_some())
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
                            if x.is_payer { "[payer]:" } else { "" },
                            x.pubkey,
                            x.lock_config
                                .as_ref()
                                .map(|x| format!(
                                    ", owner: {}, commit_frequency: {}",
                                    x.owner, x.commit_frequency
                                ))
                                .unwrap_or("".to_string()),
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
            let signature = self
                .account_cloner
                .clone_account(
                    &cloned_readonly_account.pubkey,
                    cloned_readonly_account.account,
                    None,
                )
                .await?;
            signatures.push(signature);
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
                .lock_config
                .as_ref()
                .map(|x| AccountModification {
                    owner: Some(x.owner.to_string()),
                    ..Default::default()
                });
            if cloned_writable_account.is_payer {
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
            let signature = self
                .account_cloner
                .clone_account(
                    &cloned_writable_account.pubkey,
                    cloned_writable_account.account,
                    overrides,
                )
                .await?;
            signatures.push(signature);
            // Remove the account from the readonlys and add it to writables
            self.external_readonly_accounts
                .remove(&cloned_writable_account.pubkey);
            self.external_writable_accounts.insert(
                cloned_writable_account.pubkey,
                cloned_writable_account.at_slot,
                cloned_writable_account
                    .lock_config
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
        let commit_infos = self
            .create_transactions_to_commit_specific_accounts(
                accounts_to_be_committed,
            )
            .await?;
        self.run_transactions_to_commit_specific_accounts(now, commit_infos)
            .await
    }

    pub async fn create_transactions_to_commit_specific_accounts(
        &self,
        accounts_to_be_committed: Vec<Pubkey>,
    ) -> AccountsResult<HashMap<Pubkey, CommitAccountInfo>> {
        // Get current account states from internal account provider
        let mut account_states = HashMap::new();
        for pubkey in &accounts_to_be_committed {
            let account_state =
                self.internal_account_provider.get_account(pubkey);
            if let Some(acc) = account_state {
                account_states.insert(*pubkey, acc);
            } else {
                error!(
                    "Cannot find state for account that needs to be committed '{}' ",
                    pubkey
                );
            }
        }
        // Get the transactions to commit each account
        let mut commit_infos = HashMap::new();
        for (pubkey, state) in account_states {
            let tx = self
                .account_committer
                .create_commit_account_transaction(pubkey, state.clone())
                .await?;
            let res = match tx {
                Some(tx) => {
                    CommitAccountInfo::Committed(CommitAccountTransaction {
                        transaction: tx,
                        commit_state_data: state,
                    })
                }
                None => CommitAccountInfo::NotCommitted,
            };
            commit_infos.insert(pubkey, res);
        }

        Ok(commit_infos)
    }

    pub async fn run_transactions_to_commit_specific_accounts(
        &self,
        now: Duration,
        commmit_infos: HashMap<Pubkey, CommitAccountInfo>,
    ) -> AccountsResult<Vec<Signature>> {
        let mut signatures = Vec::with_capacity(commmit_infos.len());
        // Commit the accounts and mark them as committed
        for (pubkey, info) in commmit_infos {
            use CommitAccountInfo::*;
            let CommitAccountTransaction {
                transaction,
                commit_state_data,
            } = match info {
                Committed(info) => info,
                // If the last committed state is the same as the current state
                // then it isn't committed.
                // In that case we also don't mark it as such in order to trigger
                // commitment as soon as its state changes.
                NotCommitted => continue,
            };
            let signature = self
                .account_committer
                .commit_account(pubkey, commit_state_data, transaction)
                .await?;
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
            signatures.push(signature);
        }

        Ok(signatures)
    }

    pub fn last_commit(&self, pubkey: &Pubkey) -> Option<Duration> {
        self.external_writable_accounts
            .read_accounts()
            .get(pubkey)
            .map(|x| x.last_committed_at())
    }
}
