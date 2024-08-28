use std::{
    collections::{hash_map::Entry, HashMap},
    sync::{Arc, RwLock},
    vec,
};

use conjunto_transwise::{AccountChainSnapshotShared, AccountChainState};
use futures_util::future::{join_all, ready, BoxFuture};
use log::*;
use sleipnir_account_fetcher::{AccountFetcher, AccountFetcherResult};
use sleipnir_account_updates::AccountUpdates;
use sleipnir_bank::bank::Bank;
use sleipnir_mutator::transactions::transactions_to_clone_account_from_cluster;
use sleipnir_transaction_status::TransactionStatusSender;
use solana_sdk::{
    clock::Slot,
    pubkey::{self, Pubkey},
};
use tokio::sync::{
    mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender},
    oneshot::Sender,
};
use tokio_util::sync::CancellationToken;

use crate::{AccountClonerError, AccountClonerResult};

pub struct RemoteAccountClonerWorker<AFE, AUP> {
    account_fetcher: AFE,
    account_updates: AUP,
    bank: Arc<Bank>,
    transaction_status_sender: Option<TransactionStatusSender>,
    clone_request_receiver: UnboundedReceiver<Pubkey>,
    clone_request_sender: UnboundedSender<Pubkey>,
    clone_result_listeners:
        Arc<RwLock<HashMap<Pubkey, Vec<Sender<AccountClonerResult>>>>>,
    last_cloned_snapshot:
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
    ) -> Self {
        let (clone_request_sender, clone_request_receiver) =
            unbounded_channel();
        Self {
            account_fetcher,
            account_updates,
            bank,
            transaction_status_sender,
            clone_request_receiver,
            clone_request_sender,
            clone_result_listeners: Default::default(),
            last_cloned_snapshot: Default::default(),
        }
    }

    pub fn get_clone_request_sender(&self) -> UnboundedSender<Pubkey> {
        self.clone_request_sender.clone()
    }

    pub fn get_clone_result_listeners(
        &self,
    ) -> Arc<RwLock<HashMap<Pubkey, Vec<Sender<AccountClonerResult>>>>> {
        self.clone_result_listeners.clone()
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
        let result = self.clone_if_needed(&pubkey).await;

        let listeners = match self
            .clone_result_listeners
            .write()
            .expect(
                "RwLock of RemoteAccountClonerWorker.clone_result_listeners is poisoned",
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
        for listener in listeners {
            if let Err(error) = listener.send(result.clone()) {
                error!("Could not send clone resut: {}: {:?}", pubkey, error);
            }
        }
    }

    async fn clone_if_needed(&self, pubkey: &Pubkey) -> AccountClonerResult {
        // Check for the happy/fast path, we may already have cloned this account before
        match self
            .last_cloned_snapshot
            .read()
            .expect("RwLock of RemoteAccountClonerWorker.last_cloned_snapshot is poisoned")
            .get(pubkey)
        {
            // If we cloned for the account before, check for the latest updates onchain
            Some(account_chain_snapshot) => match
                self.account_updates.get_last_known_update_slot(pubkey) {
                    // If we cloned the account and it was never updated, use the cache
                    None => return Ok(account_chain_snapshot.clone()),
                    // If we cloned the account before, check how recently
                    Some(last_known_update_slot) => match account_chain_snapshot.at_slot >= last_known_update_slot {
                        // If the cloned account is recent enough, use the cache
                        true => Ok(account_chain_snapshot.clone()),
                        // If the cloned account is too old, don't use the cache
                        false => self.do_clone(pubkey).await,
                    },
                },
            // If we never cloned the account before, don't use the cache
            None => self.do_clone(pubkey).await,
        }
    }

    async fn do_clone(&self, pubkey: &Pubkey) -> AccountClonerResult {
        // Mark the account for monitoring, we want to detect updates on it since we're cloning it
        self.account_updates
            .ensure_account_monitoring(pubkey)
            .map_err(AccountClonerError::AccountUpdatesError)?;

        // Fetch the account
        let account_chain_snapshot = self
            .account_fetcher
            .fetch_account_chain_snapshot(&pubkey)
            .await
            .map_err(AccountClonerError::AccountFetcherError)?;

        // Check bank status
        let blockhash = self.bank.last_blockhash();
        let needs_override = self.bank.get_account(pubkey).is_some();

        let txs = match account_chain_snapshot.chain_state {
            // If the account is not present on-chain, we don't need to clone anything
            AccountChainState::NewAccount => {
                vec![]
            }
            // If the account is present on-chain, but not delegated, we'd need to do some work
            AccountChainState::Undelegated { account } => {
                transactions_to_clone_account_from_cluster(
                    &self.cluster,
                    needs_override,
                    pubkey,
                    &account,
                    blockhash,
                    slot,
                    overrides,
                )
                .await?
            }
            // If the account is present and delegated on-chain, we'd need to bring it up
            AccountChainState::Delegated {
                account,
                delegation_pda,
                delegation_record,
            } => {}
            // If the account is inconsistant on-chain, we can just dump it
            AccountChainState::Inconsistent {
                account,
                delegation_pda,
                delegation_inconsistencies,
            } => {}
        };

        let signatures = txs
            .into_iter()
            .map(|clone_tx| {
                execute_legacy_transaction(
                    clone_tx,
                    &self.bank,
                    self.transaction_status_sender.as_ref(),
                )
            })
            .collect::<Result<_, _>>()?;

        // If the cloning succeeded, save it into the cache for later use
        match self
            .last_cloned_snapshot
            .write()
            .expect("RwLock of RemoteAccountClonerWorker.last_cloned_snapshot is poisoned")
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
}
