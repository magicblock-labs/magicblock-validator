use std::{
    collections::{hash_map::Entry, HashMap},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, RwLock,
    },
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
use itertools::Itertools;
use log::*;
use magicblock_account_cloner::{AccountCloner, AccountClonerOutput};
use magicblock_accounts_api::InternalAccountProvider;
use magicblock_committor_service::{
    intent_execution_manager::{
        BroadcastedIntentExecutionResult, ExecutionOutputWrapper,
    },
    intent_executor::ExecutionOutput,
    service_ext::BaseIntentCommittorExt,
    transactions::MAX_PROCESS_PER_TX,
    types::{ScheduledBaseIntentWrapper, TriggerType},
};
use magicblock_core::magic_program;
use magicblock_program::{
    magic_scheduled_base_intent::{
        CommitType, CommittedAccountV2, MagicBaseIntent, ScheduledBaseIntent,
    },
    validator::validator_authority_id,
};
use solana_sdk::{
    account::{AccountSharedData, ReadableAccount},
    hash::Hash,
    pubkey::Pubkey,
    signature::Signature,
    transaction::{SanitizedTransaction, Transaction},
};

use crate::{
    errors::{AccountsError, AccountsResult},
    utils::get_epoch,
    AccountCommittee, LifecycleMode,
};

#[derive(Debug)]
pub struct ExternalCommitableAccount {
    pubkey: Pubkey,
    owner: Pubkey,
    commit_frequency: Duration,
    last_commit_at: Duration,
    last_commit_hash: Option<Hash>,
}

impl ExternalCommitableAccount {
    pub fn new(
        pubkey: &Pubkey,
        owner: &Pubkey,
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
            owner: *owner,
            commit_frequency,
            last_commit_at,
            last_commit_hash: None,
        }
    }
    pub fn needs_commit(&self, now: &Duration) -> bool {
        *now > self.last_commit_at + self.commit_frequency
    }
    pub fn last_committed_at(&self) -> Duration {
        self.last_commit_at
    }
    pub fn mark_as_committed(&mut self, now: &Duration, hash: &Hash) {
        self.last_commit_at = *now;
        self.last_commit_hash = Some(*hash);
    }
    pub fn get_pubkey(&self) -> Pubkey {
        self.pubkey
    }
}

#[derive(Debug)]
pub struct ExternalAccountsManager<IAP, ACL, TAE, TAV, CC>
where
    IAP: InternalAccountProvider,
    ACL: AccountCloner,
    TAE: TransactionAccountsExtractor,
    TAV: TransactionAccountsValidator,
    CC: BaseIntentCommittorExt,
{
    pub internal_account_provider: IAP,
    pub account_cloner: ACL,
    pub transaction_accounts_extractor: TAE,
    pub transaction_accounts_validator: TAV,
    pub committor_service: Option<Arc<CC>>,
    pub lifecycle: LifecycleMode,
    pub external_commitable_accounts:
        RwLock<HashMap<Pubkey, ExternalCommitableAccount>>,
}

impl<IAP, ACL, TAE, TAV, CC> ExternalAccountsManager<IAP, ACL, TAE, TAV, CC>
where
    IAP: InternalAccountProvider,
    ACL: AccountCloner,
    TAE: TransactionAccountsExtractor,
    TAV: TransactionAccountsValidator,
    CC: BaseIntentCommittorExt,
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
                    Entry::Occupied(_entry) => {},
                    Entry::Vacant(entry) => {
                        entry.insert(ExternalCommitableAccount::new(
                            &account_chain_snapshot.pubkey,
                            &delegation_record.owner,
                            &delegation_record.commit_frequency,
                            &get_epoch())
                        );
                    },
                }
            }
        };
    }

    /// This will look at the time that passed since the last commit and determine
    /// which accounts are due to be committed, perform that step for them
    /// and return the signatures of the transactions that were sent to the cluster.
    pub async fn commit_delegated(
        &self,
    ) -> AccountsResult<Vec<ExecutionOutput>> {
        let Some(committor_service) = &self.committor_service else {
            return Ok(vec![]);
        };

        let now = get_epoch();
        // Find all accounts that are due to be committed let accounts_to_be_committed = self
        let accounts_to_be_committed = self
            .external_commitable_accounts
            .read()
            .expect(
                "RwLock of ExternalAccountsManager.external_commitable_accounts is poisoned",
            )
            .values()
            .flat_map(|x| {
                if x.needs_commit(&now) {
                    Some((x.get_pubkey(), x.owner, x.last_commit_hash))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        if accounts_to_be_committed.is_empty() {
            return Ok(vec![]);
        }

        // Convert committees to BaseIntents s
        let scheduled_base_intent =
            self.create_scheduled_base_intents(accounts_to_be_committed);

        // Commit BaseIntents
        let results = committor_service
            .schedule_base_intents_waiting(scheduled_base_intent.clone())
            .await?;

        // Process results
        let output = self.process_base_intents_results(
            &now,
            results,
            &scheduled_base_intent,
        );
        Ok(output)
    }

    fn process_base_intents_results(
        &self,
        now: &Duration,
        results: Vec<BroadcastedIntentExecutionResult>,
        scheduled_base_intents: &[ScheduledBaseIntentWrapper],
    ) -> Vec<ExecutionOutput> {
        // Filter failed base intents, log failed ones
        let outputs = results
            .into_iter()
            .filter_map(|execution_result| match execution_result {
                Ok(value) => Some(value),
                Err(err) => {
                    error!("Failed to send base intent: {}", err.2);
                    None
                }
            })
            .map(|output| (output.id, output))
            .collect::<HashMap<u64, ExecutionOutputWrapper>>();

        // For successfully committed accounts get their (pubkey, hash)
        let pubkeys_with_hashes = scheduled_base_intents
            .iter()
            // Filter out unsuccessful messages
            .filter(|message| outputs.contains_key(&message.inner.id))
            // Extract accounts that got committed
            .filter_map(|message| message.inner.get_committed_accounts())
            .flatten()
            // Calculate hash of committed accounts
            .map(|committed_account| {
                let acc =
                    AccountSharedData::from(committed_account.account.clone());
                let hash = hash_account(&acc);
                (committed_account.pubkey, hash)
            })
            .collect::<Vec<(Pubkey, Hash)>>();

        // Mark committed accounts
        for (pubkey, hash) in pubkeys_with_hashes {
            if let Some(acc) = self
                .external_commitable_accounts
                .write()
                .expect(
                    "RwLock of ExternalAccountsManager.external_commitable_accounts is poisoned",
                )
                .get_mut(&pubkey)
            {
                acc.mark_as_committed(now, &hash);
            }
            else {
                // This should never happen
                error!(
                    "Account '{}' disappeared while being committed",
                    pubkey
                );
            }
        }

        outputs.into_values().map(|output| output.output).collect()
    }

    fn create_scheduled_base_intents(
        &self,
        accounts_to_be_committed: Vec<(Pubkey, Pubkey, Option<Hash>)>,
    ) -> Vec<ScheduledBaseIntentWrapper> {
        // NOTE: the scheduled commits use the slot at which the commit was scheduled
        // However frequent commits run async and could be running before a slot is completed
        // Thus they really commit in between two slots instead of at the end of a particular slot.
        // Therefore we use the current slot which could result in two commits with the same
        // slot. However since we most likely will phase out frequent commits we accept this
        // inconsistency for now.
        static MESSAGE_ID: AtomicU64 = AtomicU64::new(u64::MAX - 1);

        let slot = self.internal_account_provider.get_slot();
        let blockhash = self.internal_account_provider.get_blockhash();

        // Deduce accounts that should be committed
        let committees = accounts_to_be_committed
            .iter()
            .filter_map(|(pubkey, owner, prev_hash)| {
                self.internal_account_provider.get_account(pubkey)
                    .map(|account| (pubkey, owner, prev_hash, account))
                    .or_else(|| {
                        error!("Cannot find state for account that needs to be committed '{}'", pubkey);
                        None
                    })
            })
            .filter(|(_, _, prev_hash, acc)| {
                prev_hash.map_or(true, |hash| hash_account(acc) != hash)
            })
            .map(|(pubkey, owner, _, acc)| AccountCommittee {
                pubkey: *pubkey,
                owner: *owner,
                account_data: acc,
                slot,
                undelegation_requested: false,
            })
            .collect::<Vec<_>>();

        committees
            .into_iter()
            .chunks(MAX_PROCESS_PER_TX as usize)
            .into_iter()
            .map(|committees| {
                let committees = committees
                    .map(CommittedAccountV2::from)
                    .collect::<Vec<_>>();

                ScheduledBaseIntent {
                    // isn't important but shall be unique
                    id: MESSAGE_ID.fetch_sub(1, Ordering::Relaxed),
                    slot,
                    blockhash,
                    action_sent_transaction: Transaction::default(),
                    payer: validator_authority_id(),
                    base_intent: MagicBaseIntent::Commit(
                        CommitType::Standalone(committees),
                    ),
                }
            })
            .map(|scheduled_base_intents| ScheduledBaseIntentWrapper {
                inner: scheduled_base_intents,
                trigger_type: TriggerType::OffChain,
            })
            .collect()
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
}

fn should_clone_account(pubkey: &Pubkey) -> bool {
    pubkey != &magic_program::MAGIC_CONTEXT_PUBKEY
}

/// Creates deterministic hashes from account lamports, owner and data
/// NOTE: We don't expect an account that we commit to ever change executable status, hence the
/// executable flag is not included in the hash
fn hash_account(account: &AccountSharedData) -> Hash {
    let lamports_bytes = account.lamports().to_le_bytes();
    let owner_bytes = account.owner().to_bytes();
    let data_bytes = account.data();

    let concatenated_bytes = lamports_bytes
        .iter()
        .chain(owner_bytes.iter())
        .chain(data_bytes.iter())
        .copied()
        .collect::<Vec<u8>>();

    solana_sdk::hash::hash(&concatenated_bytes)
}
