use std::{
    collections::{hash_map::Entry, HashMap, HashSet},
    sync::{Arc, RwLock},
    vec,
};

use conjunto_transwise::{AccountChainSnapshotShared, AccountChainState};
use futures_util::future::join_all;
use log::*;
use sleipnir_account_dumper::AccountDumper;
use sleipnir_account_fetcher::AccountFetcher;
use sleipnir_account_updates::AccountUpdates;
use sleipnir_accounts_api::InternalAccountProvider;
use sleipnir_mutator::idl::{get_pubkey_anchor_idl, get_pubkey_shank_idl};
use solana_sdk::{
    account::Account, bpf_loader_upgradeable::get_program_data_address,
    clock::Slot, pubkey::Pubkey, signature::Signature, system_program,
};
use tokio::sync::mpsc::{
    unbounded_channel, UnboundedReceiver, UnboundedSender,
};
use tokio_util::sync::CancellationToken;

use crate::{
    AccountClonerError, AccountClonerListeners, AccountClonerOutput,
    AccountClonerResult,
};

pub struct RemoteAccountClonerWorker<IAP, AFE, AUP, ADU> {
    internal_account_provider: IAP,
    account_fetcher: AFE,
    account_updates: AUP,
    account_dumper: ADU,
    blacklisted_accounts: HashSet<Pubkey>,
    payer_init_lamports: Option<u64>,
    allow_non_programs_undelegated: bool,
    clone_request_sender: UnboundedSender<Pubkey>,
    clone_request_receiver: UnboundedReceiver<Pubkey>,
    clone_listeners: Arc<RwLock<HashMap<Pubkey, AccountClonerListeners>>>,
    last_cloned_account_chain_snapshot:
        Arc<RwLock<HashMap<Pubkey, AccountChainSnapshotShared>>>,
    last_clone_dump_signatures: Arc<RwLock<HashMap<Pubkey, Vec<Signature>>>>,
}

impl<IAP, AFE, AUP, ADU> RemoteAccountClonerWorker<IAP, AFE, AUP, ADU>
where
    IAP: InternalAccountProvider,
    AFE: AccountFetcher,
    AUP: AccountUpdates,
    ADU: AccountDumper,
{
    pub fn new(
        internal_account_provider: IAP,
        account_fetcher: AFE,
        account_updates: AUP,
        account_dumper: ADU,
        blacklisted_accounts: HashSet<Pubkey>,
        payer_init_lamports: Option<u64>,
        allow_non_programs_undelegated: bool,
    ) -> Self {
        let (clone_request_sender, clone_request_receiver) =
            unbounded_channel();
        Self {
            internal_account_provider,
            account_fetcher,
            account_updates,
            account_dumper,
            blacklisted_accounts,
            payer_init_lamports,
            allow_non_programs_undelegated,
            clone_request_sender,
            clone_request_receiver,
            clone_listeners: Default::default(),
            last_cloned_account_chain_snapshot: Default::default(),
            last_clone_dump_signatures: Default::default(),
        }
    }

    pub fn get_clone_request_sender(&self) -> UnboundedSender<Pubkey> {
        self.clone_request_sender.clone()
    }

    pub fn get_clone_listeners(
        &self,
    ) -> Arc<RwLock<HashMap<Pubkey, AccountClonerListeners>>> {
        self.clone_listeners.clone()
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
            .clone_listeners
            .write()
            .expect(
                "RwLock of RemoteAccountClonerWorker.clone_listeners is poisoned",
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

    async fn clone_if_needed(
        &self,
        pubkey: &Pubkey,
    ) -> AccountClonerResult<AccountClonerOutput> {
        // If the account is blacklisted against cloning, no need to do anything
        if self.blacklisted_accounts.contains(pubkey) {
            return Ok(AccountClonerOutput::Unclonable(*pubkey));
        }
        // Check for the happy/fast path, we may already have cloned this account before
        match self.get_last_cloned_account_chain_snapshot(pubkey) {
            // If we cloned for the account before
            Some(snapshot) => {
                // Check for the latest updates onchain
                match self.account_updates.get_last_known_update_slot(pubkey) {
                    // If we cloned the account and it was not updated on chain, use the cache
                    None => Ok(AccountClonerOutput::Cloned(snapshot)),
                    // If we cloned the account before, but it was updated on chain, check how recently
                    Some(last_known_update_slot) => {
                        // If the cloned account is recent enough, use the cache
                        if snapshot.at_slot >= last_known_update_slot {
                            Ok(AccountClonerOutput::Cloned(snapshot))
                        }
                        // If the cloned account is too old, don't use the cache
                        else {
                            self.do_clone(pubkey).await
                        }
                    }
                }
            }
            // If we never cloned the account before, don't use the cache
            None => {
                // If somehow we already have this account in the bank, use it as is
                if self.internal_account_provider.has_account(pubkey) {
                    Ok(AccountClonerOutput::Unclonable(*pubkey))
                }
                // If we need to load it for the first time
                else {
                    self.do_clone(pubkey).await
                }
            }
        }
    }

    async fn do_clone(
        &self,
        pubkey: &Pubkey,
    ) -> AccountClonerResult<AccountClonerOutput> {
        // Mark the account for monitoring, we want to start to detect updates on it since we're cloning it now
        // TODO(vbrunet)
        //  - https://github.com/magicblock-labs/magicblock-validator/issues/95
        //  - handle the case of the lamports updates better
        //  - we may not want to track lamport changes, especially for payers
        self.account_updates
            .ensure_account_monitoring(pubkey)
            .map_err(AccountClonerError::AccountUpdatesError)?;

        // Fetch the account
        let account_chain_snapshot =
            self.fetch_account_chain_snapshot(pubkey).await?;

        // Generate cloning transactions if we need
        let signatures = match &account_chain_snapshot.chain_state {
            // If the account is not present on-chain, we don't need to clone anything
            AccountChainState::NewAccount => vec![],
            // If the account is present on-chain, but not delegated
            AccountChainState::Undelegated { account } => {
                self.do_clone_undelegated_account(pubkey, account).await?
            }
            // If the account delegated on-chain,
            // we need to apply some overrides to if we are in ephemeral mode (so it can be used as writable)
            // Otherwise we can just clone it like a regular account
            AccountChainState::Delegated {
                account,
                delegation_record,
                ..
            } => self.do_clone_delegated_account(
                pubkey,
                account,
                &delegation_record.owner,
                42, // TODO(vbrunet) - use real
            )?,
            // If the account is delegated but inconsistant on-chain,
            // We can just clone it, it won't be usable as writable,
            // but nothing stopping it from being used as a readonly
            AccountChainState::Inconsistent { account, .. } => {
                self.do_clone_undelegated_account(pubkey, account).await?
            }
        };

        // If the cloning succeeded, save it into the cache for later use
        self
            .last_cloned_account_chain_snapshot
            .write()
            .expect("RwLock of RemoteAccountClonerWorker.last_cloned_account_chain_snapshot is poisoned")
            .insert(*pubkey, account_chain_snapshot.clone());
        self
            .last_clone_dump_signatures
            .write()
            .expect("RwLock of RemoteAccountClonerWorker.last_clone_dump_signatures is poisoned")
            .insert(*pubkey, signatures);

        // Return the result
        Ok(AccountClonerOutput::Cloned(account_chain_snapshot))
    }

    async fn do_clone_undelegated_account(
        &self,
        pubkey: &Pubkey,
        account: &Account,
    ) -> AccountClonerResult<Vec<Signature>> {
        // We need to clone it if its a program, we have a special procedure.
        if account.executable {
            return self.do_clone_program(pubkey, account).await;
        }
        // In come configuration we don't allow cloning of non-programs
        if !self.allow_non_programs_undelegated {
            return Ok(vec![]);
        }
        // If it's a system account, we have a special lamport override
        if account.owner == system_program::ID {
            self.account_dumper
                .dump_system_account(pubkey, account, self.payer_init_lamports)
                .map_err(AccountClonerError::AccountDumperError)
                .map(|signature| vec![signature])
        }
        // Otherwise we just clone the account normally without any change
        else {
            self.account_dumper
                .dump_pda_account(pubkey, account)
                .map_err(AccountClonerError::AccountDumperError)
                .map(|signature| vec![signature])
        }
    }

    fn do_clone_delegated_account(
        &self,
        pubkey: &Pubkey,
        account: &Account,
        owner: &Pubkey,
        delegation_slot: Slot,
    ) -> AccountClonerResult<Vec<Signature>> {
        // If we already cloned this account from the same delegation slot
        // Keep the local state as source of truth even if it changes on-chain
        match self.get_last_cloned_account_chain_snapshot(pubkey) {
            Some(last_account_chain_snapshot) => {
                match &last_account_chain_snapshot.chain_state {
                    AccountChainState::Delegated {
                        delegation_record, ..
                    } if /* delegation_record.delegation_slot */ 42
                        == delegation_slot =>
                    {
                        return Ok(vec![]);
                    }
                    _ => {}
                }
            }
            _ => {}
        };
        // If its the first time we're seeing this delegated account, dump it to the bank
        self.account_dumper
            .dump_delegated_account(pubkey, account, owner)
            .map_err(AccountClonerError::AccountDumperError)
            .map(|signature| vec![signature])
    }

    async fn do_clone_program(
        &self,
        pubkey: &Pubkey,
        account: &Account,
    ) -> AccountClonerResult<Vec<Signature>> {
        let program_id_pubkey = pubkey;
        let program_id_account = account;
        let program_data_pubkey = &get_program_data_address(program_id_pubkey);
        let program_data_snapshot = self
            .fetch_account_chain_snapshot(program_data_pubkey)
            .await?;
        let program_data_account = program_data_snapshot
            .chain_state
            .account()
            .ok_or(AccountClonerError::ProgramDataDoesNotExist)?;
        self.account_dumper
            .dump_program_accounts(
                program_id_pubkey,
                program_id_account,
                program_data_pubkey,
                program_data_account,
                self.fetch_program_idl_snapshot(program_id_pubkey).await?,
            )
            .map_err(AccountClonerError::AccountDumperError)
    }

    async fn fetch_program_idl_snapshot(
        &self,
        program_id_pubkey: &Pubkey,
    ) -> AccountClonerResult<Option<AccountChainSnapshotShared>> {
        // First check if we can find an anchor IDL
        match get_pubkey_anchor_idl(program_id_pubkey) {
            Some(program_anchor_idl_pubkey) => {
                return Ok(Some(
                    self.fetch_account_chain_snapshot(
                        &program_anchor_idl_pubkey,
                    )
                    .await?,
                ));
            }
            None => {}
        }
        // If we coulnd't find anchor, try to find shank IDL
        match get_pubkey_shank_idl(program_id_pubkey) {
            Some(program_shank_idl_pubkey) => {
                return Ok(Some(
                    self.fetch_account_chain_snapshot(
                        &program_shank_idl_pubkey,
                    )
                    .await?,
                ));
            }
            None => {}
        }
        // Otherwise give up
        Ok(None)
    }

    async fn fetch_account_chain_snapshot(
        &self,
        pubkey: &Pubkey,
    ) -> AccountClonerResult<AccountChainSnapshotShared> {
        self.account_fetcher
            .fetch_account_chain_snapshot(pubkey)
            .await
            .map_err(AccountClonerError::AccountFetcherError)
    }

    fn get_last_cloned_account_chain_snapshot(
        &self,
        pubkey: &Pubkey,
    ) -> Option<AccountChainSnapshotShared> {
        self.last_cloned_account_chain_snapshot
            .read()
            .expect("RwLock of RemoteAccountClonerWorker.last_cloned_account_chain_snapshot is poisoned")
            .get(pubkey)
            .cloned()
    }
}
