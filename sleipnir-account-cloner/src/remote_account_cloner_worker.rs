use std::{
    collections::{hash_map::Entry, HashMap, HashSet},
    sync::{Arc, RwLock},
    vec,
};

use conjunto_transwise::{AccountChainSnapshotShared, AccountChainState};
use dlp::consts::DELEGATION_PROGRAM_ID;
use futures_util::future::join_all;
use log::*;
use sleipnir_account_dumper::AccountDumper;
use sleipnir_account_fetcher::AccountFetcher;
use sleipnir_account_updates::AccountUpdates;
use sleipnir_accounts_api::InternalAccountProvider;
use sleipnir_mutator::idl::{get_pubkey_anchor_idl, get_pubkey_shank_idl};
use solana_sdk::{
    account::{Account, ReadableAccount},
    bpf_loader_upgradeable::get_program_data_address,
    clock::Slot,
    pubkey::Pubkey,
    signature::Signature,
};
use tokio::sync::mpsc::{
    unbounded_channel, UnboundedReceiver, UnboundedSender,
};
use tokio_util::sync::CancellationToken;

use crate::{
    AccountClonerError, AccountClonerListeners, AccountClonerOutput,
    AccountClonerPermissions, AccountClonerResult,
    AccountClonerUnclonableReason,
};

pub struct RemoteAccountClonerWorker<IAP, AFE, AUP, ADU> {
    internal_account_provider: IAP,
    account_fetcher: AFE,
    account_updates: AUP,
    account_dumper: ADU,
    allowed_program_ids: Option<HashSet<Pubkey>>,
    blacklisted_accounts: HashSet<Pubkey>,
    payer_init_lamports: Option<u64>,
    permissions: AccountClonerPermissions,
    clone_request_receiver: UnboundedReceiver<Pubkey>,
    clone_request_sender: UnboundedSender<Pubkey>,
    clone_listeners: Arc<RwLock<HashMap<Pubkey, AccountClonerListeners>>>,
    last_clone_output: Arc<RwLock<HashMap<Pubkey, AccountClonerOutput>>>,
}

impl<IAP, AFE, AUP, ADU> RemoteAccountClonerWorker<IAP, AFE, AUP, ADU>
where
    IAP: InternalAccountProvider,
    AFE: AccountFetcher,
    AUP: AccountUpdates,
    ADU: AccountDumper,
{
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        internal_account_provider: IAP,
        account_fetcher: AFE,
        account_updates: AUP,
        account_dumper: ADU,
        allowed_program_ids: Option<HashSet<Pubkey>>,
        blacklisted_accounts: HashSet<Pubkey>,
        payer_init_lamports: Option<u64>,
        permissions: AccountClonerPermissions,
    ) -> Self {
        let (clone_request_sender, clone_request_receiver) =
            unbounded_channel();
        Self {
            internal_account_provider,
            account_fetcher,
            account_updates,
            account_dumper,
            allowed_program_ids,
            blacklisted_accounts,
            payer_init_lamports,
            permissions,
            clone_request_receiver,
            clone_request_sender,
            clone_listeners: Default::default(),
            last_clone_output: Default::default(),
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
        let result = self.do_clone_or_use_cache(&pubkey).await;
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
                error!("Could not send clone result: {}: {:?}", pubkey, error);
            }
        }
    }

    async fn do_clone_or_use_cache(
        &self,
        pubkey: &Pubkey,
    ) -> AccountClonerResult<AccountClonerOutput> {
        // If we don't allow any cloning, no need to do anything at all
        if !self.permissions.allow_cloning_new_accounts
            && !self.permissions.allow_cloning_payer_accounts
            && !self.permissions.allow_cloning_pda_accounts
            && !self.permissions.allow_cloning_delegated_accounts
            && !self.permissions.allow_cloning_program_accounts
        {
            return Ok(AccountClonerOutput::Unclonable {
                pubkey: *pubkey,
                reason: AccountClonerUnclonableReason::NoCloningAllowed,
                at_slot: u64::MAX, // we should never try cloning, ever
            });
        }
        // Check for the latest updates onchain for that account
        let last_known_update_slot = self
            .account_updates
            .get_last_known_update_slot(pubkey)
            .unwrap_or(u64::MIN);
        // Check for the happy/fast path, we may already have cloned this account before
        match self.get_last_clone_output(pubkey) {
            // If we already cloned this account, check what the output of the clone was
            Some(last_clone_output) => match &last_clone_output {
                // If the previous clone suceeded, we may be able to re-use it, need to check further
                AccountClonerOutput::Cloned {
                    account_chain_snapshot: snapshot,
                    ..
                } => {
                    // If the clone output is recent enough, that directly
                    if snapshot.at_slot >= last_known_update_slot {
                        // Special case temporarily to unblock zeebit:
                        // If the account is in a bork state after we somehow missed the undelegation/redelegation update
                        // We can force a bypass of the cache to get that account out of the bork state exceptionally
                        // Long-term fix tracked here: https://github.com/magicblock-labs/magicblock-validator/issues/186
                        if snapshot.chain_state.is_delegated()
                            && self
                                .internal_account_provider
                                .get_account(pubkey)
                                .map(|account| {
                                    *account.owner() == DELEGATION_PROGRAM_ID
                                })
                                .unwrap_or(false)
                        {
                            self.do_clone_and_update_cache(pubkey).await
                        }
                        // Otherwise no problem, we can use the cache
                        else {
                            Ok(last_clone_output)
                        }
                    }
                    // If the cloned account has been updated since clone, update the cache
                    else {
                        self.do_clone_and_update_cache(pubkey).await
                    }
                }
                // If the previous clone marked the account as unclonable, we may be able to re-use that output
                AccountClonerOutput::Unclonable {
                    at_slot: until_slot,
                    ..
                } => {
                    // If the clone output is recent enough, use that
                    if *until_slot >= last_known_update_slot {
                        Ok(last_clone_output)
                    }
                    // If the cloned account has been updated since clone, try to update the cache
                    else {
                        self.do_clone_and_update_cache(pubkey).await
                    }
                }
            },
            // If we never cloned the account before, we can't use the cache
            None => {
                // If somehow we already have this account in the bank, keep it as is
                if self.internal_account_provider.has_account(pubkey) {
                    Ok(AccountClonerOutput::Unclonable {
                        pubkey: *pubkey,
                        reason: AccountClonerUnclonableReason::AlreadyLocallyOverriden,
                        at_slot: u64::MAX, // we will never try cloning again
                    })
                }
                // If we need to clone it for the first time and update the cache
                else {
                    self.do_clone_and_update_cache(pubkey).await
                }
            }
        }
    }

    async fn do_clone_and_update_cache(
        &self,
        pubkey: &Pubkey,
    ) -> AccountClonerResult<AccountClonerOutput> {
        let updated_clone_output = self.do_clone(pubkey).await?;
        self.last_clone_output
            .write()
            .expect("RwLock of RemoteAccountClonerWorker.last_clone_output is poisoned")
            .insert(*pubkey, updated_clone_output.clone());
        Ok(updated_clone_output)
    }

    async fn do_clone(
        &self,
        pubkey: &Pubkey,
    ) -> AccountClonerResult<AccountClonerOutput> {
        // If the account is blacklisted against cloning, no need to do anything anytime
        if self.blacklisted_accounts.contains(pubkey) {
            return Ok(AccountClonerOutput::Unclonable {
                pubkey: *pubkey,
                reason: AccountClonerUnclonableReason::IsBlacklisted,
                at_slot: u64::MAX, // we should never try cloning again
            });
        }
        // Mark the account for monitoring, we want to start to detect futures updates on it
        // since we're cloning it now, it's now part of the validator monitored accounts
        if self.permissions.allow_cloning_refresh {
            // TODO(vbrunet)
            //  - https://github.com/magicblock-labs/magicblock-validator/issues/95
            //  - handle the case of the lamports updates better
            //  - we may not want to track lamport changes, especially for payers
            self.account_updates
                .ensure_account_monitoring(pubkey)
                .map_err(AccountClonerError::AccountUpdatesError)?;
        }
        // Fetch the account
        let account_chain_snapshot =
            self.fetch_account_chain_snapshot(pubkey).await?;
        // Generate cloning transactions
        let signature = match &account_chain_snapshot.chain_state {
            // If the account is not present on-chain
            // we may want to clear the local state
            AccountChainState::NewAccount => {
                if !self.permissions.allow_cloning_new_accounts {
                    return Ok(AccountClonerOutput::Unclonable {
                        pubkey: *pubkey,
                        reason:
                            AccountClonerUnclonableReason::DisallowNewAccount,
                        at_slot: account_chain_snapshot.at_slot,
                    });
                }
                self.do_clone_new_account(pubkey)?
            }
            // If the account is present on-chain, but not delegated
            // We need to differenciate between programs and other accounts
            AccountChainState::Undelegated { account } => {
                // If it's an executable, we may have some special fetching to do
                if account.executable {
                    if let Some(allowed_program_ids) = &self.allowed_program_ids
                    {
                        if !allowed_program_ids.contains(pubkey) {
                            return Ok(AccountClonerOutput::Unclonable {
                                pubkey: *pubkey,
                                reason: AccountClonerUnclonableReason::IsNotAllowedProgram,
                                at_slot: u64::MAX, // we will never try again
                            });
                        }
                    }
                    if !self.permissions.allow_cloning_program_accounts {
                        return Ok(AccountClonerOutput::Unclonable {
                            pubkey: *pubkey,
                            reason: AccountClonerUnclonableReason::DisallowProgramAccount,
                            at_slot: account_chain_snapshot.at_slot,
                        });
                    }
                    self.do_clone_program_accounts(pubkey, account).await?
                }
                // If it's not an executble, different rules apply depending on owner
                else {
                    // If it's a payer account, we have a special lamport override to do
                    if pubkey.is_on_curve() {
                        if !self.permissions.allow_cloning_payer_accounts {
                            return Ok(AccountClonerOutput::Unclonable {
                                pubkey: *pubkey,
                                reason: AccountClonerUnclonableReason::DisallowPayerAccount,
                                at_slot: account_chain_snapshot.at_slot,
                            });
                        }
                        self.do_clone_payer_account(pubkey, account)?
                    }
                    // Otherwise we just clone the account normally without any change
                    else {
                        if !self.permissions.allow_cloning_pda_accounts {
                            return Ok(AccountClonerOutput::Unclonable {
                                pubkey: *pubkey,
                                reason: AccountClonerUnclonableReason::DisallowPdaAccount,
                                at_slot: account_chain_snapshot.at_slot,
                            });
                        }
                        self.do_clone_pda_account(pubkey, account)?
                    }
                }
            }
            // If the account delegated on-chain, we need to apply some overrides
            // So that if we are in ephemeral mode it can be used as writable
            AccountChainState::Delegated {
                account,
                delegation_record,
                ..
            } => {
                if !self.permissions.allow_cloning_delegated_accounts {
                    return Ok(AccountClonerOutput::Unclonable {
                        pubkey: *pubkey,
                        reason:
                            AccountClonerUnclonableReason::DisallowDelegatedAccount,
                        at_slot: account_chain_snapshot.at_slot,
                    });
                }
                self.do_clone_delegated_account(
                    pubkey,
                    account,
                    &delegation_record.owner,
                    delegation_record.delegation_slot,
                )?
            }
            // If the account is delegated but inconsistant on-chain,
            // we clone it as non-delegated account to keep things simple for now
            AccountChainState::Inconsistent { account, .. } => {
                if !self.permissions.allow_cloning_pda_accounts {
                    return Ok(AccountClonerOutput::Unclonable {
                        pubkey: *pubkey,
                        reason:
                            AccountClonerUnclonableReason::DisallowPdaAccount,
                        at_slot: account_chain_snapshot.at_slot,
                    });
                }
                self.do_clone_pda_account(pubkey, account)?
            }
        };
        // Return the result
        Ok(AccountClonerOutput::Cloned {
            account_chain_snapshot,
            signature,
        })
    }

    fn do_clone_new_account(
        &self,
        pubkey: &Pubkey,
    ) -> AccountClonerResult<Signature> {
        self.account_dumper
            .dump_new_account(pubkey)
            .map_err(AccountClonerError::AccountDumperError)
    }

    fn do_clone_payer_account(
        &self,
        pubkey: &Pubkey,
        account: &Account,
    ) -> AccountClonerResult<Signature> {
        self.account_dumper
            .dump_payer_account(pubkey, account, self.payer_init_lamports)
            .map_err(AccountClonerError::AccountDumperError)
    }

    fn do_clone_pda_account(
        &self,
        pubkey: &Pubkey,
        account: &Account,
    ) -> AccountClonerResult<Signature> {
        self.account_dumper
            .dump_pda_account(pubkey, account)
            .map_err(AccountClonerError::AccountDumperError)
    }

    fn do_clone_delegated_account(
        &self,
        pubkey: &Pubkey,
        account: &Account,
        owner: &Pubkey,
        delegation_slot: Slot,
    ) -> AccountClonerResult<Signature> {
        // If we already cloned this account from the same delegation slot
        // Keep the local state as source of truth even if it changed on-chain
        if let Some(AccountClonerOutput::Cloned {
            account_chain_snapshot,
            signature,
        }) = self.get_last_clone_output(pubkey)
        {
            if let AccountChainState::Delegated {
                delegation_record, ..
            } = &account_chain_snapshot.chain_state
            {
                if delegation_record.delegation_slot == delegation_slot {
                    return Ok(signature);
                }
            }
        };
        // If its the first time we're seeing this delegated account, dump it to the bank
        self.account_dumper
            .dump_delegated_account(pubkey, account, owner)
            .map_err(AccountClonerError::AccountDumperError)
    }

    async fn do_clone_program_accounts(
        &self,
        pubkey: &Pubkey,
        account: &Account,
    ) -> AccountClonerResult<Signature> {
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
                self.fetch_program_idl(program_id_pubkey).await?,
            )
            .map_err(AccountClonerError::AccountDumperError)
    }

    async fn fetch_program_idl(
        &self,
        program_id_pubkey: &Pubkey,
    ) -> AccountClonerResult<Option<(Pubkey, Account)>> {
        // First check if we can find an anchor IDL
        let program_idl_anchor = self
            .try_fetch_program_idl_snapshot(get_pubkey_anchor_idl(
                program_id_pubkey,
            ))
            .await?;
        if program_idl_anchor.is_some() {
            return Ok(program_idl_anchor);
        }
        // If we couldn't find anchor, try to find shank IDL
        let program_idl_shank = self
            .try_fetch_program_idl_snapshot(get_pubkey_shank_idl(
                program_id_pubkey,
            ))
            .await?;
        if program_idl_shank.is_some() {
            return Ok(program_idl_shank);
        }
        // Otherwise give up
        Ok(None)
    }

    async fn try_fetch_program_idl_snapshot(
        &self,
        program_idl_pubkey: Option<Pubkey>,
    ) -> AccountClonerResult<Option<(Pubkey, Account)>> {
        if let Some(program_idl_pubkey) = program_idl_pubkey {
            let program_idl_snapshot = self
                .fetch_account_chain_snapshot(&program_idl_pubkey)
                .await?;
            let program_idl_account =
                program_idl_snapshot.chain_state.account();
            if let Some(program_idl_account) = program_idl_account {
                return Ok(Some((
                    program_idl_pubkey,
                    program_idl_account.clone(),
                )));
            }
        }
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

    fn get_last_clone_output(
        &self,
        pubkey: &Pubkey,
    ) -> Option<AccountClonerOutput> {
        self.last_clone_output
            .read()
            .expect("RwLock of RemoteAccountClonerWorker.last_clone_output is poisoned")
            .get(pubkey)
            .cloned()
    }
}
