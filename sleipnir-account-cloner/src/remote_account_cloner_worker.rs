use std::{
    collections::{hash_map::Entry, HashMap},
    sync::{Arc, RwLock},
    vec,
};

use conjunto_transwise::{
    AccountChainSnapshotProvider, AccountChainSnapshotShared,
    DelegationRecordParserImpl, RpcAccountProvider, RpcProviderConfig,
};
use futures_util::future::join_all;
use log::*;
use sleipnir_account_fetcher::AccountFetcher;
use sleipnir_account_updates::AccountUpdates;
use sleipnir_bank::bank::Bank;
use sleipnir_transaction_status::TransactionStatusSender;
use solana_sdk::pubkey::Pubkey;
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
                            .map(|request| self.do_clone(request))
                    ).await;
                }
                _ = cancellation_token.cancelled() => {
                    return;
                }
            }
        }
    }

    async fn do_clone(&self, pubkey: Pubkey) {
        let chain_snapshot = match self
            .account_fetcher
            .fetch_account_chain_snapshot(&pubkey)
            .await
        {
            Ok(snapshot) => Ok(AccountChainSnapshotShared::from(snapshot)),
            Err(error) => {
                // Log the error now, since we're going to lose the stacktrace later
                warn!("Failed to clone account: {} :{:?}", pubkey, error);
                // Lose the error content and create a simplified clonable version
                Err(AccountClonerError::FailedToClone(error.to_string()))
            }
        };

        match chain_snapshot.chain_state {
            NewAccount => {}
        };

        // TODO(vbrunet) - clone

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
            if let Err(error) = listener.send(chain_snapshot.clone()) {
                error!("Could not send clone resut: {}: {:?}", pubkey, error);
            }
        }
    }

    async fn do_clone_undelegated(&self, pubkey: Pubkey) {}
}
