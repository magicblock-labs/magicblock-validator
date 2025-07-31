use std::{
    cell::RefCell,
    collections::{hash_map::Entry, HashMap, HashSet},
    sync::{Arc, RwLock},
    time::Duration,
    vec,
};

use conjunto_transwise::{
    AccountChainSnapshot, AccountChainSnapshotShared, AccountChainState,
    DelegationRecord,
};
use futures_util::{
    future::join_all,
    stream::{self, StreamExt, TryStreamExt},
};
use log::*;
use lru::LruCache;
use magicblock_account_dumper::AccountDumper;
use magicblock_account_fetcher::AccountFetcher;
use magicblock_account_updates::{AccountUpdates, AccountUpdatesResult};
use magicblock_accounts_api::InternalAccountProvider;
use magicblock_committor_service::BaseIntentCommittor;
use magicblock_metrics::metrics;
use magicblock_mutator::idl::{get_pubkey_anchor_idl, get_pubkey_shank_idl};
use solana_sdk::{
    account::{Account, ReadableAccount},
    bpf_loader_upgradeable::{self, get_program_data_address},
    clock::Slot,
    pubkey::Pubkey,
    signature::Signature,
};
use tokio::{
    sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender},
    time::sleep,
};
use tokio_util::sync::CancellationToken;

use crate::{
    map_committor_request_result, AccountClonerError, AccountClonerListeners,
    AccountClonerOutput, AccountClonerPermissions, AccountClonerResult,
    AccountClonerUnclonableReason, CloneOutputMap,
};

pub enum ValidatorStage {
    Hydrating {
        /// The identity of our validator
        validator_identity: Pubkey,
        /// The owner of the account we consider cloning during the hydrating phase
        /// This is not really part of the validator stage, but related to a particular
        /// case of cloning an account during ledger replay.
        /// NOTE: that this will not be needed once every delegation record contains
        /// the validator authority.
        account_owner: Pubkey,
    },
    Running,
}

pub enum ValidatorCollectionMode {
    Fees,
    NoFees,
}

impl ValidatorStage {
    fn should_clone_delegated_account(
        &self,
        record: &DelegationRecord,
    ) -> bool {
        use ValidatorStage::*;
        match self {
            // If an account is delegated then one of the following is true:
            // a) it is delegated to us and we made changes to it which we should not overwrite
            //    no changes on chain were possible while it was delegated to us
            // b) it is delegated to another validator and might have changed in the meantime in
            //    which case we actually should clone it
            Hydrating {
                validator_identity,
                account_owner,
            } => {
                // If the account is delegated to us, we should not clone it
                // We can only determine this if the record.authority
                // is set to a valid address
                if record.authority.ne(&Pubkey::default()) {
                    record.authority.ne(validator_identity)
                } else {
                    // At this point the record.authority is not always set.
                    // As a workaround we check if on the account inside our validator
                    // the owner was set to the original owner of the account on chain
                    // which means it was delegated to us.
                    // If it was cloned as a readable its owner would still be the delegation
                    // program
                    account_owner.ne(&record.owner)
                }
            }
            Running => true,
        }
    }
}

pub struct RemoteAccountClonerWorker<IAP, AFE, AUP, ADU, CC> {
    internal_account_provider: IAP,
    account_fetcher: AFE,
    account_updates: AUP,
    account_dumper: ADU,
    changeset_committor: Arc<CC>,
    allowed_program_ids: Option<HashSet<Pubkey>>,
    blacklisted_accounts: HashSet<Pubkey>,
    payer_init_lamports: Option<u64>,
    validator_charges_fees: ValidatorCollectionMode,
    permissions: AccountClonerPermissions,
    fetch_retries: u64,
    clone_request_receiver: UnboundedReceiver<Pubkey>,
    clone_request_sender: UnboundedSender<Pubkey>,
    clone_listeners: Arc<RwLock<HashMap<Pubkey, AccountClonerListeners>>>,
    last_clone_output: CloneOutputMap,
    validator_identity: Pubkey,
    monitored_accounts: RefCell<LruCache<Pubkey, ()>>,
}

// SAFETY:
// we never keep references to monitored_accounts around,
// especially across await points, so this type is Send
unsafe impl<IAP, AFE, AUP, ADU, CC> Send
    for RemoteAccountClonerWorker<IAP, AFE, AUP, ADU, CC>
{
}
// SAFETY:
// we never produce references to RefCell in monitored_accounts
// especially not across await points, so this type is Sync
unsafe impl<IAP, AFE, AUP, ADU, CC> Sync
    for RemoteAccountClonerWorker<IAP, AFE, AUP, ADU, CC>
{
}

impl<IAP, AFE, AUP, ADU, CC> RemoteAccountClonerWorker<IAP, AFE, AUP, ADU, CC>
where
    IAP: InternalAccountProvider,
    AFE: AccountFetcher,
    AUP: AccountUpdates,
    ADU: AccountDumper,
    CC: BaseIntentCommittor,
{
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        internal_account_provider: IAP,
        account_fetcher: AFE,
        account_updates: AUP,
        account_dumper: ADU,
        changeset_committor: Arc<CC>,
        allowed_program_ids: Option<HashSet<Pubkey>>,
        blacklisted_accounts: HashSet<Pubkey>,
        payer_init_lamports: Option<u64>,
        validator_charges_fees: ValidatorCollectionMode,
        permissions: AccountClonerPermissions,
        validator_authority: Pubkey,
        max_monitored_accounts: usize,
    ) -> Self {
        let (clone_request_sender, clone_request_receiver) =
            unbounded_channel();
        let fetch_retries = 50;
        let max_monitored_accounts = max_monitored_accounts
            .try_into()
            .expect("max number of monitored accounts cannot be 0");
        Self {
            internal_account_provider,
            account_fetcher,
            account_updates,
            account_dumper,
            changeset_committor,
            allowed_program_ids,
            blacklisted_accounts,
            payer_init_lamports,
            validator_charges_fees,
            permissions,
            fetch_retries,
            clone_request_receiver,
            clone_request_sender,
            clone_listeners: Default::default(),
            last_clone_output: Default::default(),
            validator_identity: validator_authority,
            monitored_accounts: LruCache::new(max_monitored_accounts).into(),
        }
    }

    pub fn get_clone_request_sender(&self) -> UnboundedSender<Pubkey> {
        self.clone_request_sender.clone()
    }

    pub fn get_last_clone_output(&self) -> CloneOutputMap {
        self.last_clone_output.clone()
    }

    pub fn get_clone_listeners(
        &self,
    ) -> Arc<RwLock<HashMap<Pubkey, AccountClonerListeners>>> {
        self.clone_listeners.clone()
    }

    pub async fn start_clone_request_processing(
        mut self,
        cancellation_token: CancellationToken,
    ) {
        let mut requests = vec![];
        loop {
            tokio::select! {
                _ = self.clone_request_receiver.recv_many(&mut requests, 100) => {
                    join_all(
                        requests
                            .drain(..)
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
        let listeners = match self.clone_listeners
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

    fn can_clone(&self) -> bool {
        self.permissions.allow_cloning_feepayer_accounts
            || self.permissions.allow_cloning_undelegated_accounts
            || self.permissions.allow_cloning_delegated_accounts
            || self.permissions.allow_cloning_program_accounts
    }

    pub async fn hydrate(&self) -> AccountClonerResult<()> {
        if !self.can_clone() {
            warn!("Cloning is disabled, no need to hydrate the cache");
            return Ok(());
        }
        let account_keys = self
            .internal_account_provider
            .get_all_accounts()
            .into_iter()
            .filter(|(pubkey, _)| !self.blacklisted_accounts.contains(pubkey))
            .filter(|(pubkey, acc)| {
                // NOTE: there is an account that has ◎18,446,744,073.709553 which is present
                // at validator start. We already blacklist the faucet and validator authority and
                // therefore I don't know which account it is nor how to blacklist it.
                // The address is different every time the validator starts.
                if acc.lamports() > u64::MAX / 2 {
                    debug!("Account '{}' lamports > (u64::MAX / 2). Will not clone.", pubkey);
                    return false;
                }

                // Program accounts owned by the BPFUpgradableLoader have two parts:
                // The program and the executable data account, program account marked as `executable`.
                // The cloning pipeline already treats executable accounts specially and will
                // auto-clone the data account for each executable account. We never
                // provide the executable data account to the cloning pipeline directly (no
                // transaction ever mentions it).
                // However during hydrate we try to clone each account, including the executable
                // data which the cloning pipeline then treats as the program account and tries to
                // find its executable data account.
                // Therefore we manually remove the executable data accounts from the hydrate list
                // using the fact that only the program account is marked as executable.
                if !acc.executable() && acc.owner().eq(&bpf_loader_upgradeable::ID) {
                    return false;
                }
                true
            })
            .map(|(pubkey, acc)| (pubkey, *acc.owner()))
            .collect::<HashSet<_>>();

        let count = account_keys.len();
        debug!("Hydrating {count} accounts");
        let stream = stream::iter(account_keys);
        // NOTE: depending on the RPC provider we may get rate limited if we request
        // account states at a too high rate.
        // I confirmed the following concurrency working fine:
        //   Solana Mainnet: 10
        //   Helius: 20
        // If we go higher than this we hit 429s which causes the fetcher to have to
        // retry resulting in overall slower hydration.
        // If the optimal rate here is desired we might make this configurable in the
        // future.
        // TODO(GabrielePicco): Make the concurrency configurable
        let result = stream
            .map(Ok::<_, AccountClonerError>)
            .try_for_each_concurrent(30, |(pubkey, owner)| async move {
                trace!("Hydrating '{}'", pubkey);
                let res = self
                    .do_clone_and_update_cache(
                        &pubkey,
                        ValidatorStage::Hydrating {
                            validator_identity: self.validator_identity,
                            account_owner: owner,
                        },
                    )
                    .await;
                match res {
                    Ok(output) => {
                        trace!("Cloned '{}': {:?}", pubkey, output);
                        Ok(())
                    }
                    Err(err) => {
                        error!("Failed to clone {} ('{:?}')", pubkey, err);
                        // NOTE: the account fetch already has retries built in, so
                        // we don't to retry here

                        Err(err)
                    }
                }
            })
            .await;
        info!("On-startup account ensurance is complete: {count}");
        result
    }

    async fn do_clone_or_use_cache(
        &self,
        pubkey: &Pubkey,
    ) -> AccountClonerResult<AccountClonerOutput> {
        // If we don't allow any cloning, no need to do anything at all
        if !self.can_clone() {
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
        self.monitored_accounts.borrow_mut().promote(pubkey);
        // Check for the happy/fast path, we may already have cloned this account before
        match self.get_last_clone_output_from_pubkey(pubkey) {
            // If we already cloned this account, check what the output of the clone was
            Some(last_clone_output) => match &last_clone_output {
                // If the previous clone succeeded, we may be able to re-use it, need to check further
                AccountClonerOutput::Cloned {
                    account_chain_snapshot: snapshot,
                    ..
                } => {
                    // If the clone output is recent enough,
                    // or the account is a feepayer, we don't clone again
                    if snapshot.at_slot >= last_known_update_slot
                        || snapshot.chain_state.is_feepayer()
                    {
                        Ok(last_clone_output)
                    }
                    // If the cloned account has been updated since clone, update the cache
                    else {
                        self.do_clone_and_update_cache(
                            pubkey,
                            ValidatorStage::Running,
                        )
                        .await
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
                        self.do_clone_and_update_cache(
                            pubkey,
                            ValidatorStage::Running,
                        )
                        .await
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
                    self.do_clone_and_update_cache(
                        pubkey,
                        ValidatorStage::Running,
                    )
                    .await
                }
            }
        }
    }

    async fn do_clone_and_update_cache(
        &self,
        pubkey: &Pubkey,
        stage: ValidatorStage,
    ) -> AccountClonerResult<AccountClonerOutput> {
        let updated_clone_output = self.do_clone(pubkey, stage).await?;
        self.last_clone_output
            .write()
            .expect("RwLock of RemoteAccountClonerWorker.last_clone_output is poisoned")
            .insert(*pubkey, updated_clone_output.clone());
        if let Ok(map) = self.last_clone_output.read() {
            metrics::set_cached_clone_outputs_count(map.len());
        }
        Ok(updated_clone_output)
    }

    /// Put the account's key into cache of monitored accounts, which has a limited capacity.
    /// Once the cache capacity exceeds the preconfigured limit, it will trigger an eviction,
    /// followed by account's removal from AccountsDB and termination of its ws subscription
    async fn track_not_delegated_account(
        &self,
        pubkey: Pubkey,
    ) -> AccountUpdatesResult<()> {
        let evicted = self
            .monitored_accounts
            .borrow_mut()
            .push(pubkey, ())
            .filter(|(pk, _)| *pk != pubkey);
        if let Some((evicted, _)) = evicted {
            self.last_clone_output
                .write()
                .expect("last accounts clone output map is poisoned")
                .remove(&evicted);
            self.internal_account_provider.remove_account(&evicted);
            self.clone_listeners
                .write()
                .expect("clone listeners map is poisoned")
                .remove(&evicted);
            self.account_updates
                .stop_account_monitoring(&evicted)
                .await?;
            metrics::inc_evicted_accounts_count();
        }
        metrics::adjust_monitored_accounts_count(
            self.monitored_accounts.borrow().len(),
        );
        Ok(())
    }

    async fn do_clone(
        &self,
        pubkey: &Pubkey,
        stage: ValidatorStage,
    ) -> AccountClonerResult<AccountClonerOutput> {
        // If the account is blacklisted against cloning, no need to do anything anytime
        if self.blacklisted_accounts.contains(pubkey) {
            return Ok(AccountClonerOutput::Unclonable {
                pubkey: *pubkey,
                reason: AccountClonerUnclonableReason::IsBlacklisted,
                at_slot: u64::MAX, // we should never try cloning again
            });
        }
        // Get the latest state of the account
        let account_chain_snapshot = if self.permissions.allow_cloning_refresh {
            // Mark the account for monitoring, we want to start to detect futures updates on it
            // since we're cloning it now, it's now part of the validator monitored accounts
            // TODO(thlorenz):
            //  - https://github.com/magicblock-labs/magicblock-validator/issues/95
            //  - handle the case of the lamports updates better
            //  - we may not want to track lamport changes, especially for payers
            self.account_updates
                .ensure_account_monitoring(pubkey)
                .await?;

            // Fetch the account, repeat and retry until we have a satisfactory response
            let mut fetch_count = 0;
            loop {
                fetch_count += 1;
                let min_context_slot =
                    self.account_updates.get_first_subscribed_slot(pubkey);
                match self
                    .fetch_account_chain_snapshot(pubkey, min_context_slot)
                    .await
                {
                    Ok(account_chain_snapshot) => {
                        // We consider it a satisfactory response if the slot at which the state is from
                        // is more recent than the first successful subscription to the account
                        if account_chain_snapshot.at_slot
                            >= self
                                .account_updates
                                .get_first_subscribed_slot(pubkey)
                                .unwrap_or(u64::MAX)
                        {
                            break account_chain_snapshot;
                        }
                        // If we failed to fetch too many time, stop here
                        if fetch_count >= self.fetch_retries {
                            return if min_context_slot.is_none() {
                                Err(
                                    AccountClonerError::FailedToGetSubscriptionSlot,
                                )
                            } else {
                                Err(
                                    AccountClonerError::FailedToFetchSatisfactorySlot,
                                )
                            };
                        }
                    }
                    Err(error) => {
                        // If we failed to fetch too many time, stop here
                        if fetch_count >= self.fetch_retries {
                            return Err(error);
                        }
                    }
                };
                // Wait a bit in the hopes of the min_context_slot becoming available (about half a slot)
                sleep(Duration::from_millis(400)).await;
            }
        } else {
            self.fetch_account_chain_snapshot(pubkey, None).await?
        };
        // Generate cloning transactions
        let signature = match &account_chain_snapshot.chain_state {
            // If the account is a fee payer, we clone it assigning the init lamports of
            // the escrowed lamports (if the validator is in the charging fees mode)
            AccountChainState::FeePayer { lamports, owner } => {
                if !self.permissions.allow_cloning_feepayer_accounts {
                    return Ok(AccountClonerOutput::Unclonable {
                        pubkey: *pubkey,
                        reason: AccountClonerUnclonableReason::DoesNotAllowFeePayerAccount,
                        at_slot: account_chain_snapshot.at_slot,
                    });
                }

                // Fee payer accounts are non-delegated ones, so we keep track of them as well
                self.track_not_delegated_account(*pubkey).await?;
                match self.validator_charges_fees {
                    ValidatorCollectionMode::NoFees => self
                        .do_clone_feepayer_account_for_non_charging_validator(
                            pubkey, *lamports, owner,
                        )?,
                    ValidatorCollectionMode::Fees => {
                        // Fetch the associated escrowed account
                        let escrowed_snapshot = match self
                            .try_fetch_feepayer_chain_snapshot(pubkey, None)
                            .await?
                        {
                            Some(snapshot) => snapshot,
                            None => {
                                return Ok(AccountClonerOutput::Unclonable {
                                    pubkey: *pubkey,
                                    reason: AccountClonerUnclonableReason::DoesNotHaveEscrowAccount,
                                    at_slot: account_chain_snapshot.at_slot,
                                });
                            }
                        };

                        let escrowed_account = match escrowed_snapshot
                            .chain_state
                            .account()
                        {
                            Some(account) => account,
                            None => {
                                return Ok(AccountClonerOutput::Unclonable {
                                    pubkey: *pubkey,
                                    reason: AccountClonerUnclonableReason::DoesNotHaveDelegatedEscrowAccount,
                                    at_slot: escrowed_snapshot.at_slot,
                                });
                            }
                        };

                        // Add the escrowed account as unclonable.
                        // Fail cloning if the account is already present.
                        // This prevents escrow PDA from being cloned if the lamports are mapped to the feepayer.
                        {
                            let mut last_clone_output = self
                                .last_clone_output
                                .write()
                                .expect("RwLock of RemoteAccountClonerWorker.last_clone_output is poisoned");

                            match last_clone_output
                                .entry(escrowed_snapshot.pubkey)
                            {
                                Entry::Occupied(_) => {
                                    return Ok(AccountClonerOutput::Unclonable {
                                        pubkey: *pubkey,
                                        reason: AccountClonerUnclonableReason::DoesNotAllowFeepayerWithEscrowedPda,
                                        at_slot: account_chain_snapshot.at_slot,
                                    });
                                }
                                Entry::Vacant(entry) => {
                                    entry.insert(AccountClonerOutput::Unclonable {
                                        pubkey: escrowed_snapshot.pubkey,
                                        reason: AccountClonerUnclonableReason::DoesNotAllowEscrowedPda,
                                        at_slot: Slot::MAX,
                                    });
                                }
                            }
                        }

                        self.do_clone_feepayer_account(
                            pubkey,
                            escrowed_account.lamports,
                            owner,
                            Some(&escrowed_snapshot.pubkey),
                        )?
                    }
                }
            }
            // If the account is present on-chain, but not delegated, it's just readonly data
            // We need to differenciate between programs and other accounts
            AccountChainState::Undelegated { account, .. } => {
                // If it's an executable, we may have some special fetching to do
                if account.executable {
                    if let Some(allowed_program_ids) = &self.allowed_program_ids
                    {
                        if !allowed_program_ids.contains(pubkey) {
                            return Ok(AccountClonerOutput::Unclonable {
                                pubkey: *pubkey,
                                reason: AccountClonerUnclonableReason::IsNotAnAllowedProgram,
                                at_slot: u64::MAX, // we will never try again
                            });
                        }
                    }
                    if !self.permissions.allow_cloning_program_accounts {
                        return Ok(AccountClonerOutput::Unclonable {
                            pubkey: *pubkey,
                            reason: AccountClonerUnclonableReason::DoesNotAllowProgramAccount,
                            at_slot: account_chain_snapshot.at_slot,
                        });
                    }
                    self.do_clone_program_accounts(
                        pubkey,
                        account,
                        Some(account_chain_snapshot.at_slot),
                    )
                    .await?
                }
                // If it's not an executable, simpler rules apply
                else {
                    if !self.permissions.allow_cloning_undelegated_accounts {
                        return Ok(AccountClonerOutput::Unclonable {
                            pubkey: *pubkey,
                            reason: AccountClonerUnclonableReason::DoesNotAllowUndelegatedAccount,
                            at_slot: account_chain_snapshot.at_slot,
                        });
                    }
                    // Keep track of non-delegated accounts, removing any stale ones,
                    // which were evicted from monitored accounts cache
                    self.track_not_delegated_account(*pubkey).await?;
                    self.do_clone_undelegated_account(pubkey, account)?
                }
            }
            // If the account delegated on-chain, we need to apply some overrides
            // So that if we are in ephemeral mode it can be used as writable
            AccountChainState::Delegated {
                account,
                delegation_record,
                ..
            } => {
                // Just in case if the account was promoted from not delegated to delegated state, we
                // remove it from list of monitored accounts, to avoid removal on eviction
                self.monitored_accounts.borrow_mut().pop(pubkey);
                metrics::adjust_monitored_accounts_count(
                    self.monitored_accounts.borrow().len(),
                );

                if !self.permissions.allow_cloning_delegated_accounts {
                    return Ok(AccountClonerOutput::Unclonable {
                        pubkey: *pubkey,
                        reason:
                        AccountClonerUnclonableReason::DoesNotAllowDelegatedAccount,
                        at_slot: account_chain_snapshot.at_slot,
                    });
                }
                if !stage.should_clone_delegated_account(delegation_record)
                    && self
                        .internal_account_provider
                        .get_account(pubkey)
                        .is_some_and(|acc| {
                            acc.owner().eq(&delegation_record.owner)
                        })
                {
                    // NOTE: the account was already cloned when the initial instance of this
                    // validator ran. We don't want to clone it again during ledger replay, however
                    // we want to use it as a delegated + cloned account, thus we respond in the
                    // same manner as we just cloned it.
                    // Unfortunately we don't know the signature, but during ledger replay
                    // this should not be too important.
                    return Ok(AccountClonerOutput::Cloned {
                        account_chain_snapshot,
                        signature: Signature::new_unique(),
                    });
                }

                let sig = self.do_clone_delegated_account(
                    pubkey,
                    // TODO(GabrielePicco): Avoid cloning
                    &Account {
                        lamports: delegation_record.lamports,
                        ..account.clone()
                    },
                    delegation_record,
                )?;

                // Allow the committer service to reserve pubkeys in lookup tables
                // that could be needed when we commit this account
                map_committor_request_result(
                    self.changeset_committor.reserve_pubkeys_for_committee(
                        *pubkey,
                        delegation_record.owner,
                    ),
                )
                .await?;

                sig
            }
        };
        // Return the result
        Ok(AccountClonerOutput::Cloned {
            account_chain_snapshot,
            signature,
        })
    }

    fn do_clone_feepayer_account(
        &self,
        pubkey: &Pubkey,
        lamports: u64,
        owner: &Pubkey,
        balance_pda: Option<&Pubkey>,
    ) -> AccountClonerResult<Signature> {
        self.account_dumper
            .dump_feepayer_account(pubkey, lamports, owner)
            .map_err(AccountClonerError::AccountDumperError)
            .inspect(|_| {
                metrics::inc_account_clone(metrics::AccountClone::FeePayer {
                    pubkey: &pubkey.to_string(),
                    balance_pda: balance_pda.map(|p| p.to_string()).as_deref(),
                });
            })
    }

    /// Clone a fee payer account setting the initial lamports to payer_init_lamports
    fn do_clone_feepayer_account_for_non_charging_validator(
        &self,
        pubkey: &Pubkey,
        lamports: u64,
        owner: &Pubkey,
    ) -> AccountClonerResult<Signature> {
        let lamports = self.payer_init_lamports.unwrap_or(lamports);
        self.do_clone_feepayer_account(pubkey, lamports, owner, None)
    }

    fn do_clone_undelegated_account(
        &self,
        pubkey: &Pubkey,
        account: &Account,
    ) -> AccountClonerResult<Signature> {
        self.account_dumper
            .dump_undelegated_account(pubkey, account)
            .map_err(AccountClonerError::AccountDumperError)
            .inspect(|_| {
                metrics::inc_account_clone(
                    metrics::AccountClone::Undelegated {
                        pubkey: &pubkey.to_string(),
                        owner: &account.owner().to_string(),
                    },
                );
            })
    }

    fn do_clone_delegated_account(
        &self,
        pubkey: &Pubkey,
        account: &Account,
        record: &DelegationRecord,
    ) -> AccountClonerResult<Signature> {
        // If we already cloned this account from the same delegation slot
        // Keep the local state as source of truth even if it changed on-chain
        if let Some(AccountClonerOutput::Cloned {
            account_chain_snapshot,
            signature,
        }) = self.get_last_clone_output_from_pubkey(pubkey)
        {
            if let AccountChainState::Delegated {
                delegation_record, ..
            } = &account_chain_snapshot.chain_state
            {
                if delegation_record.delegation_slot == record.delegation_slot {
                    return Ok(signature);
                }
            }
        };
        // If its the first time we're seeing this delegated account, dump it to the bank
        self.account_dumper
            .dump_delegated_account(pubkey, account, &record.owner)
            .map_err(AccountClonerError::AccountDumperError)
            .inspect(|_| {
                metrics::inc_account_clone(metrics::AccountClone::Delegated {
                    // TODO(bmuddha): optimize metrics, remove .to_string()
                    pubkey: &pubkey.to_string(),
                    owner: &record.owner.to_string(),
                });
            })
    }

    async fn do_clone_program_accounts(
        &self,
        pubkey: &Pubkey,
        account: &Account,
        min_context_slot: Option<Slot>,
    ) -> AccountClonerResult<Signature> {
        let program_id_pubkey = pubkey;
        let program_id_account = account;

        // NOTE: first versions of BPF loader didn't store program in a separate
        // executable account, using program account instead and thus couldn't upgrade program.
        // As such, only use executable account derivation and cloning for upgradable BPF loader
        // https://github.com/magicblock-labs/magicblock-validator/issues/130
        if account.owner == solana_sdk::bpf_loader_deprecated::ID {
            // FIXME(bmuddha13): once deprecated loader becomes available in magic validator,
            // clone such programs like normal accounts
            return Err(AccountClonerError::ProgramDataDoesNotExist);
        } else if account.owner == solana_sdk::bpf_loader::ID {
            let signature =
                self.account_dumper.dump_program_account_with_old_bpf(
                    program_id_pubkey,
                    program_id_account,
                )?;
            return Ok(signature);
        }

        let program_data_pubkey = &get_program_data_address(program_id_pubkey);
        let program_data_snapshot = self
            .fetch_account_chain_snapshot(program_data_pubkey, min_context_slot)
            .await?;
        let program_data_account = program_data_snapshot
            .chain_state
            .account()
            .ok_or(AccountClonerError::ProgramDataDoesNotExist)?;
        let idl_account = match self
            .fetch_program_idl(program_id_pubkey, min_context_slot)
            .await?
        {
            // Only add the IDL account if it exists on chain
            Some((pubkey, account)) if account.lamports > 0 => {
                Some((pubkey, account))
            }
            _ => None,
        };
        self.account_dumper
            .dump_program_accounts(
                program_id_pubkey,
                program_id_account,
                program_data_pubkey,
                program_data_account,
                idl_account,
            )
            .map_err(AccountClonerError::AccountDumperError)
            .inspect(|_| {
                metrics::inc_account_clone(metrics::AccountClone::Program {
                    pubkey: &pubkey.to_string(),
                });
            })
    }

    async fn fetch_program_idl(
        &self,
        program_id_pubkey: &Pubkey,
        min_context_slot: Option<Slot>,
    ) -> AccountClonerResult<Option<(Pubkey, Account)>> {
        // First check if we can find an anchor IDL
        let program_idl_anchor = self
            .try_fetch_program_idl_snapshot(
                get_pubkey_anchor_idl(program_id_pubkey),
                min_context_slot,
            )
            .await?;
        if program_idl_anchor.is_some() {
            return Ok(program_idl_anchor);
        }
        // If we couldn't find anchor, try to find shank IDL
        let program_idl_shank = self
            .try_fetch_program_idl_snapshot(
                get_pubkey_shank_idl(program_id_pubkey),
                min_context_slot,
            )
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
        min_context_slot: Option<Slot>,
    ) -> AccountClonerResult<Option<(Pubkey, Account)>> {
        if let Some(program_idl_pubkey) = program_idl_pubkey {
            let program_idl_snapshot = self
                .fetch_account_chain_snapshot(
                    &program_idl_pubkey,
                    min_context_slot,
                )
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
        min_context_slot: Option<Slot>,
    ) -> AccountClonerResult<AccountChainSnapshotShared> {
        self.account_fetcher
            .fetch_account_chain_snapshot(pubkey, min_context_slot)
            .await
            .map_err(AccountClonerError::AccountFetcherError)
    }

    async fn try_fetch_feepayer_chain_snapshot(
        &self,
        feepayer: &Pubkey,
        min_context_slot: Option<Slot>,
    ) -> AccountClonerResult<Option<AccountChainSnapshotShared>> {
        let account_snapshot = self
            .account_fetcher
            .fetch_account_chain_snapshot(
                &AccountChainSnapshot::ephemeral_balance_pda(feepayer),
                min_context_slot,
            )
            .await
            .map_err(AccountClonerError::AccountFetcherError)?;
        if let AccountChainState::Delegated {
            account: _,
            delegation_record,
            ..
        } = &account_snapshot.chain_state
        {
            // TODO(GabrielePicco): remove the Pubkey::default() option once we enforce the authority to be always set
            if delegation_record.authority == self.validator_identity
                || delegation_record.authority == Pubkey::default()
            {
                return Ok(Some(account_snapshot));
            }
        }
        Ok(None)
    }

    fn get_last_clone_output_from_pubkey(
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
