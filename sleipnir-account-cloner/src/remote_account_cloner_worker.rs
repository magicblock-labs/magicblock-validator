use std::{
    collections::{hash_map::Entry, HashMap, HashSet},
    sync::{Arc, RwLock},
    vec,
};

use conjunto_transwise::{
    AccountChainSnapshot, AccountChainSnapshotShared, AccountChainState,
    DelegationRecord,
};
use futures_util::future::join_all;
use log::*;
use sleipnir_account_fetcher::AccountFetcher;
use sleipnir_account_updates::AccountUpdates;
use sleipnir_bank::bank::Bank;
use sleipnir_mutator::{
    program::{create_program_modifications, ProgramModifications},
    transactions::{
        transaction_to_clone_regular_account, transactions_to_clone_program,
    },
    AccountModification,
};
use sleipnir_processor::execute_transaction::execute_legacy_transaction;
use sleipnir_transaction_status::TransactionStatusSender;
use solana_sdk::{
    account::Account,
    bpf_loader_upgradeable::get_program_data_address,
    compute_budget,
    pubkey::Pubkey,
    signature::Signature,
    system_program, sysvar,
    transaction::{Result, Transaction},
};
use tokio::sync::{
    mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender},
    oneshot::Sender,
};
use tokio_util::sync::CancellationToken;

use crate::{AccountClonerError, AccountClonerOutput, AccountClonerResult};

pub struct RemoteAccountClonerWorker<AFE, AUP> {
    account_fetcher: AFE,
    account_updates: AUP,
    bank: Arc<Bank>,
    transaction_status_sender: Option<TransactionStatusSender>,
    blacklisted_accounts: HashSet<Pubkey>,
    payer_init_lamports: Option<u64>,
    allow_non_programs_undelegated: bool,
    clone_request_receiver: UnboundedReceiver<Pubkey>,
    clone_request_sender: UnboundedSender<Pubkey>,
    clone_output_listeners:
        Arc<RwLock<HashMap<Pubkey, Vec<Sender<AccountClonerOutput>>>>>,
    clone_output_cache:
        Arc<RwLock<HashMap<Pubkey, AccountChainSnapshotShared>>>,
}

impl<AFE, AUP> RemoteAccountClonerWorker<AFE, AUP>
where
    AFE: AccountFetcher,
    AUP: AccountUpdates,
{
    pub fn new(
        account_fetcher: AFE,
        account_updates: AUP,
        bank: Arc<Bank>,
        transaction_status_sender: Option<TransactionStatusSender>,
        validator_id: &Pubkey,
        payer_init_lamports: Option<u64>,
        allow_non_programs_undelegated: bool,
    ) -> Self {
        let (clone_request_sender, clone_request_receiver) =
            unbounded_channel();
        let mut blacklisted_accounts = HashSet::new();
        blacklisted_accounts.insert(sysvar::clock::ID);
        blacklisted_accounts.insert(sysvar::epoch_rewards::ID);
        blacklisted_accounts.insert(sysvar::epoch_schedule::ID);
        blacklisted_accounts.insert(sysvar::fees::ID);
        blacklisted_accounts.insert(sysvar::instructions::ID);
        blacklisted_accounts.insert(sysvar::last_restart_slot::ID);
        blacklisted_accounts.insert(sysvar::recent_blockhashes::ID);
        blacklisted_accounts.insert(sysvar::rent::ID);
        blacklisted_accounts.insert(sysvar::rewards::ID);
        blacklisted_accounts.insert(sysvar::slot_hashes::ID);
        blacklisted_accounts.insert(sysvar::slot_history::ID);
        blacklisted_accounts.insert(sysvar::stake_history::ID);
        blacklisted_accounts.insert(compute_budget::ID);
        blacklisted_accounts.insert(*validator_id);
        Self {
            account_fetcher,
            account_updates,
            bank,
            transaction_status_sender,
            blacklisted_accounts,
            payer_init_lamports,
            allow_non_programs_undelegated,
            clone_request_receiver,
            clone_request_sender,
            clone_output_listeners: Default::default(),
            clone_output_cache: Default::default(),
        }
    }

    pub fn get_clone_request_sender(&self) -> UnboundedSender<Pubkey> {
        self.clone_request_sender.clone()
    }

    pub fn get_clone_output_listeners(
        &self,
    ) -> Arc<RwLock<HashMap<Pubkey, Vec<Sender<AccountClonerOutput>>>>> {
        self.clone_output_listeners.clone()
    }

    pub async fn start_clone_request_processing(
        &mut self,
        cancellation_token: CancellationToken,
    ) {
        loop {
            let mut requests = vec![];
            tokio::select! {
                _ = self.clone_request_receiver.recv_many(&mut requests, 100) => {
                    join_all(
                        requests
                            .into_iter()
                            .map(|request| self.process_clone_request(request))
                    ).await;
                }
                _ = cancellation_token.cancelled() => {
                    return;
                }
            }
        }
    }

    async fn process_clone_request(&self, pubkey: Pubkey) {
        // Actually run the whole cloning process on the bank, yield until done
        let result = self.clone_if_needed(&pubkey).await;
        // Collecting the list of listeners awaiting for the clone to be done
        let listeners = match self
            .clone_output_listeners
            .write()
            .expect(
                "RwLock of RemoteAccountClonerWorker.clone_output_listeners is poisoned",
            )
            .entry(pubkey)
        {
            // If the entry didn't exist for some reason, something is very wrong, just fail here
            Entry::Vacant(_) => {
                return error!("Clone listeners were discarded improperly: {}", pubkey);
            }
            // If the entry exists, we want to consume the list of listeners
            Entry::Occupied(entry) => entry.remove(),
        };
        // Notify every listeners of the clone's result
        for listener in listeners {
            if let Err(error) = listener.send(result.clone()) {
                error!("Could not send clone resut: {}: {:?}", pubkey, error);
            }
        }
    }

    async fn clone_if_needed(&self, pubkey: &Pubkey) -> AccountClonerOutput {
        // If the account is blacklisted against cloning, no need to do anything
        if self.blacklisted_accounts.contains(pubkey) {
            return self.skipped_clone_output(pubkey);
        }
        // Check for the happy/fast path, we may already have cloned this account before
        match self.get_last_clone_output(pubkey) {
            // If we cloned for the account before, check for the latest updates onchain
            Some(account_chain_snapshot) => {
                match self.account_updates.get_last_known_update_slot(pubkey) {
                    // If we cloned the account and it was never updated, use the cache
                    None => return Ok(account_chain_snapshot.clone()),
                    // If we cloned the account before, check how recently
                    Some(last_known_update_slot) => {
                        match account_chain_snapshot.at_slot
                            >= last_known_update_slot
                        {
                            // If the cloned account is recent enough, use the cache
                            true => Ok(account_chain_snapshot.clone()),
                            // If the cloned account is too old, don't use the cache
                            false => self.do_clone(pubkey).await,
                        }
                    }
                }
            }
            // If we never cloned the account before, don't use the cache
            None => {
                // If somehow we already have this account in the bank, use it as is
                if self.bank.has_account(pubkey) {
                    self.skipped_clone_output(pubkey)
                }
                // If we need to load it for the first time
                else {
                    self.do_clone(pubkey).await
                }
            }
        }
    }

    async fn do_clone(&self, pubkey: &Pubkey) -> AccountClonerOutput {
        // Mark the account for monitoring, we want to start to detect updates on it since we're cloning it now
        // TODO(vbrunet)
        //  - https://github.com/magicblock-labs/magicblock-validator/issues/95
        //  - handle the case of the lamports updates better
        //  - we may not want to track lamport changes, especially for payers
        self.account_updates
            .ensure_account_monitoring(pubkey)
            .map_err(AccountClonerError::AccountUpdatesError)?;

        // Fetch the account
        let account_chain_snapshot = self.fetch_account(&pubkey).await?;

        // Generate cloning transactions if we need
        let txs = match &account_chain_snapshot.chain_state {
            // If the account is not present on-chain, we don't need to clone anything
            AccountChainState::NewAccount => vec![
                // TODO(vbrunet)
                // - we may need to make sure we wipe this account locally?
                // - since we cant create account in ephemerals this might not be a problem?
            ],
            // If the account is present on-chain, but not delegated
            // We need to clone it if its a program, we have a special procedure.
            // If it is not a program, we can just clone it without overrides
            AccountChainState::Undelegated { account } => {
                if account.executable {
                    self.build_transactions_for_program_cloning(
                        pubkey, &account,
                    )
                    .await?
                } else {
                    self.build_transaction_for_regular_account_cloning(
                        pubkey, &account,
                    )
                }
            }
            // If the account delegated on-chain,
            // we need to apply some overrides to if we are in ephemeral mode (so it can be used as writable)
            // Otherwise we can just clone it like a regular account
            AccountChainState::Delegated {
                account,
                delegation_record,
                ..
            } => self.build_transaction_for_delegated_account_cloning(
                pubkey,
                &account,
                delegation_record,
            ),
            // If the account is delegated but inconsistant on-chain,
            // We can just clone it, it won't be usable as writable,
            // but nothing stopping it from being used as a readonly
            AccountChainState::Inconsistent { account, .. } => self
                .build_transaction_for_regular_account_cloning(
                    pubkey, &account,
                ),
        };

        // Execute the cloning transactions in the bank
        let _signatures = txs
            .into_iter()
            .map(|clone_tx| {
                execute_legacy_transaction(
                    clone_tx,
                    &self.bank,
                    self.transaction_status_sender.as_ref(),
                )
            })
            .collect::<Result<Vec<Signature>>>()
            .map_err(AccountClonerError::TransactionError)?;

        // If the cloning succeeded, save it into the cache for later use
        match self
            .clone_output_cache
            .write()
            .expect("RwLock of RemoteAccountClonerWorker.clone_output_cache is poisoned")
            .entry(*pubkey)
        {
            Entry::Occupied(mut entry) => {
                *entry.get_mut() = account_chain_snapshot.clone();
            }
            Entry::Vacant(entry) => {
                entry.insert(account_chain_snapshot.clone());
            },
        };

        // Return the result
        Ok(account_chain_snapshot)
    }

    async fn build_transactions_for_program_cloning(
        &self,
        pubkey: &Pubkey,
        account: &Account,
    ) -> AccountClonerResult<Vec<Transaction>> {
        let program_id_pubkey = pubkey;
        let program_id_account = account;

        let program_data_pubkey = &get_program_data_address(program_id_pubkey);
        let program_data_snapshot =
            self.fetch_account(&program_data_pubkey).await?;
        let program_data_account = program_data_snapshot
            .chain_state
            .account()
            .ok_or(AccountClonerError::ProgramDataDoesNotExist)?;

        let ProgramModifications {
            program_id_modification,
            program_data_modification,
            program_buffer_modification,
        } = create_program_modifications(
            program_id_pubkey,
            program_id_account,
            program_data_pubkey,
            program_data_account,
            self.bank.slot(),
        )
        .map_err(AccountClonerError::MutatorModificationError)?;

        let needs_upgrade = self.bank.get_account(pubkey).is_some();
        Ok(transactions_to_clone_program(
            needs_upgrade,
            program_id_modification,
            program_data_modification,
            program_buffer_modification,
            None, // TODO(vbrunet) - fetch IDL too
            self.bank.last_blockhash(),
        ))
    }

    fn build_transaction_for_regular_account_cloning(
        &self,
        pubkey: &Pubkey,
        account: &Account,
    ) -> Vec<Transaction> {
        // We may want to disable cloning undelegated regular accounts
        if !self.allow_non_programs_undelegated {
            return vec![];
        }
        // If this is an account that may be used as a payer, airdrop lamports to it.
        // This logic will be replaced once we implement value reconcilation
        let overrides = match self.payer_init_lamports {
            Some(payer_init_lamports) => {
                if account.owner == system_program::ID {
                    Some(AccountModification {
                        pubkey: *pubkey,
                        lamports: Some(account.lamports + payer_init_lamports),
                        ..Default::default()
                    })
                } else {
                    None
                }
            }
            None => None,
        };
        // Simply mutate the account to match the on-chain state
        vec![transaction_to_clone_regular_account(
            pubkey,
            account,
            overrides,
            self.bank.last_blockhash(),
        )]
    }

    fn build_transaction_for_delegated_account_cloning(
        &self,
        pubkey: &Pubkey,
        account: &Account,
        delegation_record: &DelegationRecord,
    ) -> Vec<Transaction> {
        let last_clone_output = self.get_last_clone_output(pubkey);
        if last_clone_output.is_some() {
            // If we already cloned the delegated account, make sure to not re-clone it
            return vec![];
        }

        let overrides = Some(AccountModification {
            pubkey: *pubkey,
            owner: Some(delegation_record.owner),
            ..Default::default()
        });
        // Simply mutate the account to match the on-chain state
        vec![transaction_to_clone_regular_account(
            pubkey,
            account,
            overrides,
            self.bank.last_blockhash(),
        )]
    }

    async fn fetch_account(&self, pubkey: &Pubkey) -> AccountClonerOutput {
        self.account_fetcher
            .fetch_account_chain_snapshot(&pubkey)
            .await
            .map_err(AccountClonerError::AccountFetcherError)
    }

    fn get_last_clone_output(
        &self,
        pubkey: &Pubkey,
    ) -> Option<AccountChainSnapshotShared> {
        self.clone_output_cache.read()
        .expect("RwLock of RemoteAccountClonerWorker.clone_output_cache is poisoned")
        .get(pubkey).cloned()
    }

    fn skipped_clone_output(&self, pubkey: &Pubkey) -> AccountClonerOutput {
        Ok(AccountChainSnapshot {
            pubkey: *pubkey,
            at_slot: self.bank.slot(),
            chain_state: AccountChainState::NewAccount,
        }
        .into())
    }
}
