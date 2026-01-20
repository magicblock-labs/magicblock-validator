use std::{
    collections::HashSet,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use borsh::BorshDeserialize;
use compressed_delegation_client::CompressedDelegationRecord;
use dlp::{
    pda::delegation_record_pda_from_delegated_account, state::DelegationRecord,
};
use magicblock_config::config::AllowedProgram;
use magicblock_core::traits::AccountsBank;
use magicblock_metrics::metrics::{self, AccountFetchOrigin};
use scc::{hash_map::Entry, HashMap};
use solana_account::{AccountSharedData, ReadableAccount, WritableAccount};
use solana_pubkey::Pubkey;
use solana_sdk_ids::system_program;
use tokio::{
    sync::{mpsc, oneshot},
    task,
    task::JoinSet,
};
use tracing::*;

mod ata_projection;
mod delegation;
mod pipeline;
mod program_loader;
mod subscription;
mod types;

pub use self::types::FetchAndCloneResult;
use self::{
    subscription::{cancel_subs, CancelStrategy},
    types::{
        AccountWithCompanion, ClassifiedAccounts, ExistingSubs,
        PartitionedNotFound, RefreshDecision, ResolvedDelegatedAccounts,
        ResolvedPrograms,
    },
};
use super::errors::{ChainlinkError, ChainlinkResult};
use crate::{
    chainlink::{
        account_still_undelegating_on_chain::account_still_undelegating_on_chain,
        blacklisted_accounts::blacklisted_accounts,
    },
    cloner::{errors::ClonerResult, AccountCloneRequest, Cloner},
    remote_account_provider::{
        photon_client::PhotonClient,
        program_account::get_loaderv3_get_program_data_address,
        ChainPubsubClient, ChainRpcClient, ForwardedSubscriptionUpdate,
        MatchSlotsConfig, RemoteAccount, RemoteAccountProvider,
        ResolvedAccountSharedData,
    },
};

type RemoteAccountRequests = Vec<oneshot::Sender<()>>;

#[derive(Clone)]
pub struct FetchCloner<T, U, V, C, P>
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
    V: AccountsBank,
    C: Cloner,
    P: PhotonClient,
{
    /// The RemoteAccountProvider to fetch accounts from
    remote_account_provider: Arc<RemoteAccountProvider<T, U, P>>,
    /// Tracks pending account fetch requests to avoid duplicate fetches in parallel
    /// Once an account is fetched and cloned into the bank, it's removed from here
    pending_requests: Arc<HashMap<Pubkey, RemoteAccountRequests>>,
    /// Counter to track the number of fetch operations for testing deduplication
    fetch_count: Arc<AtomicU64>,

    accounts_bank: Arc<V>,
    cloner: Arc<C>,
    validator_pubkey: Pubkey,

    /// These are accounts that we should never clone into our validator.
    /// native programs, sysvars, native tokens, validator identity and faucet
    blacklisted_accounts: HashSet<Pubkey>,

    /// If specified, only these programs will be cloned. If None or empty,
    /// all programs are allowed.
    allowed_programs: Option<HashSet<Pubkey>>,
}

impl<T, U, V, C, P> FetchCloner<T, U, V, C, P>
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
    V: AccountsBank,
    C: Cloner,
    P: PhotonClient,
{
    /// Create FetchCloner with subscription updates properly connected
    pub fn new(
        remote_account_provider: &Arc<RemoteAccountProvider<T, U, P>>,
        accounts_bank: &Arc<V>,
        cloner: &Arc<C>,
        validator_pubkey: Pubkey,
        faucet_pubkey: Pubkey,
        subscription_updates_rx: mpsc::Receiver<ForwardedSubscriptionUpdate>,
        allowed_programs: Option<Vec<AllowedProgram>>,
    ) -> Arc<Self> {
        let blacklisted_accounts =
            blacklisted_accounts(&validator_pubkey, &faucet_pubkey);
        let allowed_programs = allowed_programs.map(|programs| {
            programs.iter().map(|p| p.id).collect::<HashSet<_>>()
        });
        let me = Arc::new(Self {
            remote_account_provider: remote_account_provider.clone(),
            accounts_bank: accounts_bank.clone(),
            cloner: cloner.clone(),
            validator_pubkey,
            pending_requests: Arc::new(HashMap::new()),
            fetch_count: Arc::new(AtomicU64::new(0)),
            blacklisted_accounts,
            allowed_programs,
        });

        me.clone()
            .start_subscription_listener(subscription_updates_rx);

        me
    }

    /// Get the current fetch count
    pub fn fetch_count(&self) -> u64 {
        self.fetch_count.load(Ordering::Relaxed)
    }

    /// Check if a program is allowed to be cloned.
    /// Returns true if:
    /// - No allowed_programs restriction is set (None), OR
    /// - The allowed_programs set is empty (treats empty as unrestricted), OR
    /// - The program is in the allowed_programs set
    fn is_program_allowed(&self, program_id: &Pubkey) -> bool {
        match &self.allowed_programs {
            None => true,
            Some(allowed) => {
                if allowed.is_empty() {
                    true
                } else {
                    allowed.contains(program_id)
                }
            }
        }
    }

    /// Start listening to subscription updates
    pub fn start_subscription_listener(
        self: Arc<Self>,
        mut subscription_updates: mpsc::Receiver<ForwardedSubscriptionUpdate>,
    ) {
        tokio::spawn(async move {
            while let Some(update) = subscription_updates.recv().await {
                let pubkey = update.pubkey;
                let slot = update.account.slot();
                trace!(pubkey = %pubkey, slot, "FetchCloner received subscription update");

                // Process each subscription update concurrently to avoid blocking on delegation
                // record fetches. This allows multiple updates to be processed in parallel.
                let this = Arc::clone(&self);
                tokio::spawn(async move {
                    let (resolved_account, deleg_record) =
                    this.resolve_account_to_clone_from_forwarded_sub_with_unsubscribe(update)
                    .await;
                    if let Some(account) = resolved_account {
                        // Ensure that the subscription update isn't out of order, i.e. we don't already
                        // hold a newer version of the account in our bank
                        let out_of_order_slot = this
                            .accounts_bank
                            .get_account(&pubkey)
                            .and_then(|in_bank| {
                                if in_bank.remote_slot()
                                    >= account.remote_slot()
                                {
                                    Some(in_bank.remote_slot())
                                } else {
                                    None
                                }
                            });
                        if let Some(in_bank_slot) = out_of_order_slot {
                            let update_slot = account.remote_slot();
                            trace!(
                                pubkey = %pubkey,
                                bank_slot = in_bank_slot,
                                update_slot,
                                "Ignoring out-of-order subscription update"
                            );
                            return;
                        }

                        if let Some(in_bank) =
                            this.accounts_bank.get_account(&pubkey)
                        {
                            if in_bank.undelegating() {
                                // We expect the account to still be delegated, but with the delegation
                                // program owner
                                debug!(
                                    pubkey = %pubkey,
                                    in_bank_delegated = in_bank.delegated(),
                                    in_bank_owner = %in_bank.owner(),
                                    in_bank_slot = in_bank.remote_slot(),
                                    chain_delegated = account.delegated(),
                                    chain_owner = %account.owner(),
                                    chain_slot = account.remote_slot(),
                                    "Received update for undelegating account"
                                );

                                // This will only be true in the following case:
                                // 1. a commit was triggered for the account
                                // 2. a commit + undelegate was triggered for the account -> undelegating
                                // 3. we receive the update for (1.)
                                //
                                // Thus our state is more up to date and we don't need to update our
                                // bank.
                                if account_still_undelegating_on_chain(
                                    &pubkey,
                                    account.delegated(),
                                    in_bank.remote_slot(),
                                    deleg_record,
                                    &this.validator_pubkey,
                                ) {
                                    return;
                                }
                            } else if in_bank.owner().eq(&dlp::id()) {
                                debug!(
                                    pubkey = %pubkey,
                                    "Received update for account owned by delegation program but not marked as undelegating"
                                );
                            }
                        } else {
                            warn!(
                                pubkey = %pubkey,
                                "Received update for account not in bank"
                            );
                        }

                        // Determine if delegated to another validator
                        let delegated_to_other = deleg_record
                            .as_ref()
                            .and_then(|dr| this.get_delegated_to_other(dr));

                        // Once we clone an account that is delegated to us we no longer need
                        // to receive updates for it from chain
                        // The subscription will be turned back on once the committor service schedules
                        // a commit for it that includes undelegation
                        if account.delegated() {
                            if let Err(err) = this
                                .remote_account_provider
                                .unsubscribe(&pubkey)
                                .await
                            {
                                error!(
                                    pubkey = %pubkey,
                                    error = %err,
                                    "Failed to unsubscribe from delegated account"
                                );
                            }
                        }

                        if account.executable() {
                            this.handle_executable_sub_update(pubkey, account)
                                .await;
                        } else if let Err(err) = this
                            .cloner
                            .clone_account(AccountCloneRequest {
                                pubkey,
                                account,
                                commit_frequency_ms: None,
                                delegated_to_other,
                            })
                            .await
                        {
                            error!(
                                pubkey = %pubkey,
                                error = %err,
                                "Failed to clone account into bank"
                            );
                        }
                    }
                });
            }
        });
    }

    async fn handle_executable_sub_update(
        &self,
        pubkey: Pubkey,
        account: AccountSharedData,
    ) {
        // moved to program_loader module
        program_loader::handle_executable_sub_update(self, pubkey, account)
            .await;
    }

    async fn resolve_account_to_clone_from_forwarded_sub_with_unsubscribe(
        &self,
        update: ForwardedSubscriptionUpdate,
    ) -> (Option<AccountSharedData>, Option<DelegationRecord>) {
        let ForwardedSubscriptionUpdate { pubkey, account } = update;
        let owned_by_delegation_program =
            account.is_owned_by_delegation_program();
        let owned_by_compressed_delegation_program =
            account.is_owned_by_compressed_delegation_program();

        if let Some(mut account) = account.fresh_account().cloned() {
            // If the account is owned by the delegation program we need to resolve
            // its true owner and determine if it is delegated to us
            if owned_by_delegation_program {
                let delegation_record_pubkey =
                    delegation_record_pda_from_delegated_account(&pubkey);

                // Check existing subscriptions before fetching
                let was_delegation_record_subscribed = self
                    .remote_account_provider
                    .is_watching(&delegation_record_pubkey);

                match self
                    .task_to_fetch_with_companion(
                        pubkey,
                        delegation_record_pubkey,
                        account.remote_slot(),
                        AccountFetchOrigin::GetAccount,
                    )
                    .await
                {
                    Ok(Ok(AccountWithCompanion {
                        pubkey,
                        mut account,
                        companion_pubkey: delegation_record_pubkey,
                        companion_account: delegation_record,
                    })) => {
                        // We need to remove subs for the delegation record and the account
                        // if it is delegated to us
                        let mut subs_to_remove = HashSet::new();

                        // Always unsubscribe from delegation record if it was a new subscription
                        if !was_delegation_record_subscribed {
                            subs_to_remove.insert(delegation_record_pubkey);
                        }

                        let account = if let Some(delegation_record) =
                            delegation_record
                        {
                            let delegation_record =
                                match Self::parse_delegation_record(
                                    delegation_record.data(),
                                    delegation_record_pubkey,
                                ) {
                                    Ok(x) => Some(x),
                                    Err(err) => {
                                        error!(
                                            pubkey = %pubkey,
                                            error = %err,
                                            "Failed to parse delegation record"
                                        );
                                        None
                                    }
                                };

                            // If the delegation record is valid we set the owner and delegation
                            // status on the account
                            if let Some(delegation_record) = delegation_record {
                                if tracing::enabled!(tracing::Level::TRACE) {
                                    let delegation_record_display =
                                        format!("{:?}", delegation_record);
                                    trace!(
                                        pubkey = %pubkey,
                                        slot = account.remote_slot(),
                                        owner = %delegation_record.owner,
                                        deleg_record = %delegation_record_display,
                                        "Resolving delegated account"
                                    );
                                }

                                self.apply_delegation_record_to_account(
                                    &mut account,
                                    &delegation_record,
                                );

                                // For accounts delegated to us, always unsubscribe from the delegated account
                                if account.delegated() {
                                    subs_to_remove.insert(pubkey);
                                }

                                (
                                    Some(account.into_account_shared_data()),
                                    Some(delegation_record),
                                )
                            } else {
                                // If the delegation record is invalid we cannot clone the account
                                // since something is corrupt and we wouldn't know what owner to
                                // use, etc.
                                (None, None)
                            }
                        } else {
                            // If no delegation record exists we must assume the account itself is
                            // a delegation record or metadata
                            (Some(account.into_account_shared_data()), None)
                        };

                        if !subs_to_remove.is_empty() {
                            cancel_subs(
                                &self.remote_account_provider,
                                CancelStrategy::All(subs_to_remove),
                            )
                            .await;
                        }
                        account
                    }
                    // In case of errors fetching the delegation record we cannot clone the account
                    Ok(Err(err)) => {
                        error!(
                            pubkey = %pubkey,
                            error = %err,
                            "Failed to fetch delegation record"
                        );
                        (None, None)
                    }
                    Err(err) => {
                        error!(
                            pubkey = %pubkey,
                            error = %err,
                            "Failed to fetch delegation record"
                        );
                        (None, None)
                    }
                }
            } else if owned_by_compressed_delegation_program {
                // If the account is compressed, it can contain either:
                // 1. The delegation record
                // 2. No data in case the last update was the notif where the account was emptied
                // If we fail to get the record, we need to fetch again so that we obtain the data
                // from the compressed account.
                let delegation_record =
                    match CompressedDelegationRecord::try_from_slice(
                        account.data(),
                    ) {
                        Ok(delegation_record) => Some(delegation_record),
                        Err(parse_err) => {
                            debug!("The account's data did not contain a valid compressed delegation record for {pubkey}: {parse_err}, fetching...");
                            match self
                                .remote_account_provider
                                .try_get(pubkey, AccountFetchOrigin::GetAccount)
                                .await
                            {
                                Ok(remote_acc) => {
                                    if let Some(acc) =
                                        remote_acc.fresh_account().cloned()
                                    {
                                        match CompressedDelegationRecord::try_from_slice(acc.data()) {
                                            Ok(delegation_record) => Some(delegation_record),
                                            Err(parse_err) => {
                                                error!("fetched account parse failed for {pubkey}: {parse_err}");
                                                None
                                            }
                                        }
                                    } else {
                                        error!("remote fetch failed for {pubkey}: no fresh account returned");
                                        None
                                    }
                                }
                                Err(fetch_err) => {
                                    error!("remote fetch failed for {pubkey}: {fetch_err}");
                                    None
                                }
                            }
                        }
                    };

                if let Some(delegation_record) = delegation_record {
                    account.set_compressed(true);
                    account.set_owner(delegation_record.owner);
                    account.set_data(delegation_record.data);
                    account.set_lamports(delegation_record.lamports);
                    account.set_confined(
                        delegation_record.authority.eq(&Pubkey::default()),
                    );

                    let is_delegated_to_us =
                        delegation_record.authority.eq(&self.validator_pubkey);
                    account.set_delegated(is_delegated_to_us);

                    // TODO(dode): commit frequency ms is not supported for compressed delegation records
                    (
                        Some(account),
                        Some(DelegationRecord {
                            authority: delegation_record.authority,
                            owner: delegation_record.owner,
                            delegation_slot: delegation_record.delegation_slot,
                            lamports: delegation_record.lamports,
                            commit_frequency_ms: 0,
                        }),
                    )
                } else {
                    (None, None)
                }
            } else {
                // Accounts not owned by the delegation program can be cloned as is
                // No unsubscription needed for undelegated accounts
                (Some(account), None)
            }
        } else {
            // This should not happen since we call this method with sub updates which always hold
            // a fresh remote account
            error!(pubkey = %pubkey, account = ?account, "BUG: Received subscription update without fresh account");
            (None, None)
        }
    }

    /// Parses a delegation record from account data bytes.
    /// Returns the parsed DelegationRecord, or InvalidDelegationRecord error
    /// if parsing fails.
    fn parse_delegation_record(
        data: &[u8],
        delegation_record_pubkey: Pubkey,
    ) -> ChainlinkResult<DelegationRecord> {
        delegation::parse_delegation_record(data, delegation_record_pubkey)
    }

    /// Applies delegation record settings to an account: sets the owner,
    /// delegation status, and confined status based on the delegation
    /// record's authority field.
    /// Returns commit frequency if account is delegated to us
    fn apply_delegation_record_to_account(
        &self,
        account: &mut ResolvedAccountSharedData,
        delegation_record: &DelegationRecord,
    ) -> Option<u64> {
        delegation::apply_delegation_record_to_account(
            self,
            account,
            delegation_record,
        )
    }

    /// Returns the pubkey of another validator if account is delegated to them,
    /// None if delegated to us or delegated to the system program (confined).
    fn get_delegated_to_other(
        &self,
        delegation_record: &DelegationRecord,
    ) -> Option<Pubkey> {
        delegation::get_delegated_to_other(self, delegation_record)
    }

    /// Fetches and parses the delegation record for an account, returning the
    /// parsed DelegationRecord if found and valid, None otherwise.
    async fn fetch_and_parse_delegation_record(
        &self,
        account_pubkey: Pubkey,
        min_context_slot: u64,
        fetch_origin: metrics::AccountFetchOrigin,
    ) -> Option<DelegationRecord> {
        delegation::fetch_and_parse_delegation_record(
            self,
            account_pubkey,
            min_context_slot,
            fetch_origin,
        )
        .await
    }

    /// Tries to fetch all accounts in `pubkeys` and clone them into the bank.
    /// If `mark_empty` is provided, accounts in that list that are
    /// not found on chain will be added with zero lamports to the bank.
    ///
    /// - **pubkeys**: list of accounts to fetch and clone
    /// - **mark_empty**: optional list of accounts that should be added as empty if not found on
    ///   chain
    /// - **slot**: optional slot to use as minimum context slot for the accounts being cloned
    ///
    /// NOTE: accounts fetched here have not been found in the bank
    #[instrument(skip(self, pubkeys, mark_empty_if_not_found, program_ids))]
    async fn fetch_and_clone_accounts(
        &self,
        pubkeys: &[Pubkey],
        mark_empty_if_not_found: Option<&[Pubkey]>,
        slot: Option<u64>,
        fetch_origin: AccountFetchOrigin,
        program_ids: Option<&[Pubkey]>,
    ) -> ChainlinkResult<FetchAndCloneResult> {
        if tracing::enabled!(tracing::Level::TRACE) {
            let pubkeys_count = pubkeys.len();
            trace!(count = pubkeys_count, "Fetching and cloning accounts");
        }

        // We keep all existing subscriptions including delegation records and program data
        // accounts that were directly requested
        let ExistingSubs { existing_subs } =
            pipeline::build_existing_subs(self, pubkeys);

        // Track all new subscriptions created during this call
        let mut new_subs: HashSet<Pubkey> = HashSet::new();

        // Increment fetch counter for testing deduplication (count per account being fetched)
        self.fetch_count
            .fetch_add(pubkeys.len() as u64, Ordering::Relaxed);

        let accs = self
            .remote_account_provider
            .try_get_multi(
                pubkeys,
                mark_empty_if_not_found,
                fetch_origin,
                program_ids,
            )
            .await?;

        // Fetching accounts creates subscriptions for all requested pubkeys
        new_subs.extend(pubkeys.iter().copied());

        if tracing::enabled!(tracing::Level::TRACE) {
            let accs_count = accs.len();
            trace!(count = accs_count, "Fetched accounts");
        }

        let ClassifiedAccounts {
            not_found,
            plain,
            owned_by_deleg,
            owned_by_deleg_compressed,
            programs,
            atas,
        } = pipeline::classify_remote_accounts(accs, pubkeys);

        if tracing::enabled!(tracing::Level::TRACE) {
            let not_found = not_found
                .iter()
                .map(|(pubkey, slot)| (pubkey.to_string(), *slot))
                .collect::<Vec<_>>();
            let plain = plain
                .iter()
                .map(|p| p.pubkey.to_string())
                .collect::<Vec<_>>();
            let owned_by_deleg = owned_by_deleg
                .iter()
                .map(|(pubkey, _, slot)| (pubkey.to_string(), *slot))
                .collect::<Vec<_>>();
            let owned_by_deleg_compressed = owned_by_deleg_compressed
                .iter()
                .map(|(pubkey, _, slot)| (pubkey.to_string(), *slot))
                .collect::<Vec<_>>();
            let programs = programs
                .iter()
                .map(|(p, _, _)| p.to_string())
                .collect::<Vec<_>>();
            let atas = atas
                .iter()
                .map(|(a, _, _, _)| a.to_string())
                .collect::<Vec<_>>();
            trace!(
                "Fetched accounts: \nnot_found:      {not_found:?} \nplain:          {plain:?} \nowned_by_deleg: {owned_by_deleg:?} \nowned_by_deleg_compressed: {owned_by_deleg_compressed:?} \nprograms:       {programs:?} \natas:       {atas:?}",
            );
        }

        let PartitionedNotFound {
            clone_as_empty,
            not_found,
        } = pipeline::partition_not_found(mark_empty_if_not_found, not_found);

        // For accounts we couldn't find we cannot do anything. We will let code depending
        // on them to be in the bank fail on its own
        if !not_found.is_empty() {
            trace!(
                "Could not find accounts on chain: {:?}",
                not_found
                    .iter()
                    .map(|(pubkey, slot)| (pubkey.to_string(), *slot))
                    .collect::<Vec<_>>()
            );
        }

        // We mark some accounts as empty if we know that they will never exist on chain
        if tracing::enabled!(tracing::Level::TRACE)
            && !clone_as_empty.is_empty()
        {
            trace!(
                "Cloning accounts as empty: {:?}",
                clone_as_empty
                    .iter()
                    .map(|(p, _)| p.to_string())
                    .collect::<Vec<_>>()
            );
        }

        // Calculate min context slot: use the greater of subscription slot or last chain slot
        let min_context_slot = slot.map(|subscription_slot| {
            subscription_slot.max(self.remote_account_provider.chain_slot())
        });

        // For potentially delegated accounts we update the owner and delegation state first
        let ResolvedDelegatedAccounts {
            mut accounts_to_clone,
            record_subs,
            missing_delegation_record,
        } = pipeline::resolve_delegated_accounts(
            self,
            owned_by_deleg,
            plain,
            min_context_slot,
            fetch_origin,
            pubkeys,
            existing_subs.clone(),
        )
        .await?;

        // Track delegation record subscriptions
        new_subs.extend(record_subs.iter().copied());

        let ResolvedPrograms {
            loaded_programs,
            program_data_subs,
        } = pipeline::resolve_programs_with_program_data(
            self,
            programs,
            min_context_slot,
            fetch_origin,
            pubkeys,
            existing_subs.clone(),
        )
        .await?;

        // Track program data account subscriptions
        new_subs.extend(program_data_subs.iter().copied());

        // We will compute subscription cancellations after ATA handling, once accounts_to_clone is finalized

        // Handle ATAs: for each detected ATA, we derive the eATA PDA, subscribe to both,
        // and, if the ATA is delegated to us and the eATA exists, we clone the eATA data
        // into the ATA in the bank.
        // Note: ATA subscriptions are already in new_subs (from pubkeys).
        // eATA subscriptions are kept implicitly (not tracked for cancellation).
        let ata_accounts = ata_projection::resolve_ata_with_eata_projection(
            self,
            atas,
            min_context_slot,
            fetch_origin,
        )
        .await;
        accounts_to_clone.extend(ata_accounts);

        // Handle compressed delegated accounts: for each compressed delegated account, we clone the account data into the bank.
        let compressed_delegated_accounts =
            pipeline::resolve_compressed_delegated_accounts(
                self,
                owned_by_deleg_compressed,
            )
            .await?;
        accounts_to_clone.extend(compressed_delegated_accounts);

        // Compute sub cancellations now since we may potentially fail during a cloning step
        let cancel_strategy = pipeline::compute_cancel_strategy(
            pubkeys,
            &accounts_to_clone,
            &loaded_programs,
            record_subs,
            program_data_subs,
            existing_subs,
            new_subs,
        );

        cancel_subs(&self.remote_account_provider, cancel_strategy).await;

        pipeline::clone_accounts_and_programs(
            self,
            accounts_to_clone,
            loaded_programs,
        )
        .await?;

        Ok(FetchAndCloneResult {
            not_found_on_chain: not_found,
            missing_delegation_record,
        })
    }

    /// Determines if the account finished undelegating on chain.
    /// If it has finished undelegating, we should refresh it in the bank.
    /// - **pubkey**: the account pubkey
    /// - **in_bank**: the account as it exists in the bank
    ///
    /// Returns true if the account should be refreshed in the bank
    async fn should_refresh_undelegating_in_bank_account(
        &self,
        pubkey: &Pubkey,
        in_bank: &AccountSharedData,
        fetch_origin: AccountFetchOrigin,
    ) -> RefreshDecision {
        if in_bank.compressed() {
            let Some(record) =
                CompressedDelegationRecord::from_bytes(in_bank.data()).ok()
            else {
                // The delegation record is not present in the account data because the actual account data
                // has already been extracted from it. Refreshing would reset the account, losing local changes.
                debug!(
                    pubkey = %pubkey,
                    data = %format!("{:?}", in_bank.data()),
                    "Skip refresh for already processed compressed account"
                );
                return RefreshDecision::No;
            };

            if !account_still_undelegating_on_chain(
                pubkey,
                record.authority.eq(&self.validator_pubkey)
                    || record.authority.eq(&Pubkey::default()),
                in_bank.remote_slot(),
                Some(DelegationRecord {
                    authority: record.authority,
                    owner: record.owner,
                    delegation_slot: record.delegation_slot,
                    lamports: record.lamports,
                    commit_frequency_ms: 0, // TODO(dode): use the actual commit frequency once implemented
                }),
                &self.validator_pubkey,
            ) {
                debug!(
                    pubkey = %pubkey,
                    "Refresh compressed account since the compressed delegation record is not undelegating"
                );
                return RefreshDecision::Yes;
            };
            debug!(
                pubkey = %pubkey,
                "Skip refresh for compressed account since the compressed delegation record is undelegating"
            );
        } else if in_bank.undelegating() {
            debug!(
                pubkey = %pubkey,
                delegated = in_bank.delegated(),
                undelegating = in_bank.undelegating(),
                "Fetching undelegating account"
            );
            let deleg_record = self
                .fetch_and_parse_delegation_record(
                    *pubkey,
                    self.remote_account_provider.chain_slot(),
                    fetch_origin,
                )
                .await;

            if deleg_record.is_none() {
                // If there is no delegation record then it is possible that the account itself
                // does not exist either.
                // In that case we need to refresh it as empty to clear the undelegation state.
                return RefreshDecision::YesAndMarkEmptyIfNotFound;
            }

            let delegated_on_chain = deleg_record.as_ref().is_some_and(|dr| {
                dr.authority.eq(&self.validator_pubkey)
                    || dr.authority.eq(&Pubkey::default())
            });
            if !account_still_undelegating_on_chain(
                pubkey,
                delegated_on_chain,
                in_bank.remote_slot(),
                deleg_record,
                &self.validator_pubkey,
            ) {
                debug!(
                    "Account {pubkey} marked as undelegating will be overridden since undelegation completed"
                );
                return RefreshDecision::Yes;
            }
        } else if in_bank.owner().eq(&dlp::id()) {
            debug!(
                "Account {pubkey} owned by deleg program not marked as undelegating"
            );
        }
        RefreshDecision::No
    }

    /// Fetch and clone accounts with request deduplication to avoid parallel fetches of the same account.
    /// This method implements the new logic where:
    /// 1. Check synchronously if account is in bank, return immediately if found
    /// 2. If account is pending, add to pending requests and await
    /// 3. Create pending entries and fetch via RemoteAccountProvider
    /// 4. Once fetched, clone into bank and respond to all pending requests
    /// 5. Clear pending requests for that account
    ///
    /// Note: since we fetch each account only once in parallel, we also avoid fetching
    /// the same delegation record in parallel.
    #[instrument(skip(self, pubkeys, mark_empty_if_not_found, program_ids))]
    pub async fn fetch_and_clone_accounts_with_dedup(
        &self,
        pubkeys: &[Pubkey],
        mark_empty_if_not_found: Option<&[Pubkey]>,
        slot: Option<u64>,
        fetch_origin: AccountFetchOrigin,
        program_ids: Option<&[Pubkey]>,
    ) -> ChainlinkResult<FetchAndCloneResult> {
        // We cannot clone blacklisted accounts, thus either they are already
        // in the bank (e.g. native programs) or they don't exist and the transaction
        // will fail later
        let mut pubkeys = pubkeys
            .iter()
            .filter(|p| !self.blacklisted_accounts.contains(p))
            .collect::<Vec<_>>();
        if tracing::enabled!(tracing::Level::TRACE) {
            let count = pubkeys.len();
            trace!(count, "Fetching and cloning accounts with dedup");
        }

        let mut await_pending = vec![];
        let mut fetch_new = vec![];
        let mut in_bank = vec![];
        let mut extra_mark_empty = vec![];
        for pubkey in pubkeys.iter() {
            if let Some(account_in_bank) =
                self.accounts_bank.get_account(pubkey)
            {
                let decision = match tokio::time::timeout(
                    Duration::from_secs(5),
                    self.should_refresh_undelegating_in_bank_account(
                        pubkey,
                        &account_in_bank,
                        fetch_origin,
                    ),
                )
                .await
                {
                    Ok(decision) => decision,
                    Err(_timeout) => {
                        warn!(
                            pubkey = %pubkey,
                            "Timeout checking if account is still undelegating after 5 seconds"
                        );
                        RefreshDecision::No
                    }
                };

                match decision {
                    RefreshDecision::Yes
                    | RefreshDecision::YesAndMarkEmptyIfNotFound => {
                        debug!(
                            pubkey = %pubkey,
                            "Account completed undelegation which was missed and is fetched again"
                        );
                        metrics::inc_unstuck_undelegation_count();
                        if let RefreshDecision::YesAndMarkEmptyIfNotFound =
                            decision
                        {
                            extra_mark_empty.push(*pubkey);
                        }
                    }
                    RefreshDecision::No => {
                        // Account is in bank and subscribed correctly - no fetch needed
                        if tracing::enabled!(tracing::Level::TRACE) {
                            let undelegating = account_in_bank.undelegating();
                            let delegated = account_in_bank.delegated();
                            let owner = account_in_bank.owner();
                            trace!(
                                pubkey = %pubkey,
                                undelegating,
                                delegated,
                                owner = %owner,
                                "Account found in bank in valid state, no fetch needed"
                            );
                        }
                        in_bank.push(*pubkey);
                    }
                }
            }
        }
        pubkeys.retain(|p| !in_bank.contains(p));

        // Check pending requests and bank synchronously
        for pubkey in pubkeys {
            // Check if account fetch is already pending
            match self.pending_requests.entry(*pubkey) {
                Entry::Occupied(mut requests) => {
                    let (sender, receiver) = oneshot::channel();
                    requests.get_mut().push(sender);
                    await_pending.push((*pubkey, receiver));
                }
                Entry::Vacant(e) => {
                    // Reserve an entry for the new fetch request
                    e.insert_entry(vec![]);
                    // Account needs to be fetched - add to fetch list
                    fetch_new.push(*pubkey);
                }
            }
        }

        // If we have accounts to fetch, delegate to the existing implementation
        // but notify all pending requests when done
        let result = if !fetch_new.is_empty() {
            let mut all_mark_empty = mark_empty_if_not_found
                .map(|x| x.to_vec())
                .unwrap_or_default();
            all_mark_empty.extend(extra_mark_empty);
            let mark_empty_ref = if all_mark_empty.is_empty() {
                None
            } else {
                Some(all_mark_empty.as_slice())
            };

            self.fetch_and_clone_accounts(
                &fetch_new,
                mark_empty_ref,
                slot,
                fetch_origin,
                program_ids,
            )
            .await
        } else {
            Ok(FetchAndCloneResult {
                not_found_on_chain: vec![],
                missing_delegation_record: vec![],
            })
        };

        // Clear pending requests for fetched accounts - pending requesters can get
        // the accounts from the bank now since fetch_and_clone_accounts succeeded
        for &pubkey in &fetch_new {
            if let Some((_, requests)) = self.pending_requests.remove(&pubkey) {
                // We signal completion but don't send the actual account data since:
                // 1. The account is now in the bank if it was successfully cloned
                // 2. If there was an error, the result will contain the error info
                // 3. Pending requesters can check the bank or result as needed
                for sender in requests {
                    let _ = sender.send(());
                }
            }
        }

        // Wait for any pending requests to complete
        let mut joinset = JoinSet::new();
        for (pubkey, receiver) in await_pending {
            joinset.spawn(async move {
                if let Err(err) = receiver
                    .await
                    .inspect_err(|err| {
                        warn!(pubkey = %pubkey, error = ?err, "FetchCloner::clone_accounts - RecvError awaiting account, sender dropped without sending value");
                    })
                {
                    // The sender was dropped, likely due to an error in the other request
                    error!(
                        "Failed to receive account from pending request: {err}"
                    );
                }
            });
        }
        joinset.join_all().await;

        result
    }

    fn task_to_fetch_with_delegation_record(
        &self,
        pubkey: Pubkey,
        slot: u64,
        fetch_origin: AccountFetchOrigin,
    ) -> task::JoinHandle<ChainlinkResult<AccountWithCompanion>> {
        let delegation_record_pubkey =
            delegation_record_pda_from_delegated_account(&pubkey);
        self.task_to_fetch_with_companion(
            pubkey,
            delegation_record_pubkey,
            slot,
            fetch_origin,
        )
    }

    fn task_to_fetch_with_program_data(
        &self,
        pubkey: Pubkey,
        slot: u64,
        fetch_origin: AccountFetchOrigin,
    ) -> task::JoinHandle<ChainlinkResult<AccountWithCompanion>> {
        let program_data_pubkey =
            get_loaderv3_get_program_data_address(&pubkey);
        self.task_to_fetch_with_companion(
            pubkey,
            program_data_pubkey,
            slot,
            fetch_origin,
        )
    }

    fn task_to_fetch_with_companion(
        &self,
        pubkey: Pubkey,
        companion_pubkey: Pubkey,
        slot: u64,
        fetch_origin: AccountFetchOrigin,
    ) -> task::JoinHandle<ChainlinkResult<AccountWithCompanion>> {
        let provider = self.remote_account_provider.clone();
        let bank = self.accounts_bank.clone();
        let fetch_count = self.fetch_count.clone();
        task::spawn(async move {
            trace!(
                pubkey = %pubkey,
                companion = %companion_pubkey,
                slot,
                "Fetching account with companion"
            );

            // Increment fetch counter for testing deduplication (2 accounts: pubkey + delegation_record_pubkey)
            fetch_count.fetch_add(2, Ordering::Relaxed);

            provider
                .try_get_multi_until_slots_match(
                    &[pubkey, companion_pubkey],
                    Some(MatchSlotsConfig {
                        min_context_slot: Some(slot),
                        ..Default::default()
                    }),
                    fetch_origin,
                )
                .await
                .map_err(ChainlinkError::from)
                .and_then(|accs| {
                    match accs.as_slice() {
                        [acc_first, acc_last] => {
                            Ok((acc_first.clone(), acc_last.clone()))
                        }
                        _ => Err(ChainlinkError::UnexpectedAccountCount(format!(
                            "Expected exactly 2 accounts for pubkey {} and companion {}, got {}",
                            pubkey,
                            companion_pubkey,
                            accs.len()
                        ))),
                    }
                })
                .and_then(|(acc, deleg)| {
                    Self::resolve_account_with_companion(
                        &bank,
                        pubkey,
                        companion_pubkey,
                        acc,
                        deleg,
                    )
                })
        })
    }

    fn resolve_account_with_companion(
        bank: &V,
        pubkey: Pubkey,
        companion_pubkey: Pubkey,
        acc: RemoteAccount,
        companion: RemoteAccount,
    ) -> ChainlinkResult<AccountWithCompanion> {
        use RemoteAccount::*;
        match (acc, companion) {
            // Account not found even though we found it previously - this is invalid,
            // either way we cannot use it now
            (NotFound(_), NotFound(_)) | (NotFound(_), Found(_)) => {
                Err(ChainlinkError::ResolvedAccountCouldNoLongerBeFound(pubkey))
            }
            (Found(acc), NotFound(_)) => {
                // Only account found without a companion
                // In case of delegation record fetch the account is either invalid
                // or a delegation record itself.
                // Clone it as is (without changing the owner or flagging as delegated)
                match acc.account.resolved_account_shared_data(bank) {
                    Some(account) => Ok(AccountWithCompanion {
                        pubkey,
                        account,
                        companion_pubkey,
                        companion_account: None,
                    }),
                    None => Err(
                        ChainlinkError::ResolvedAccountCouldNoLongerBeFound(
                            pubkey,
                        ),
                    ),
                }
            }
            (Found(acc), Found(comp)) => {
                // Found the delegation record, we include it so that the caller can
                // use it to add metadata to the account and use it for decision making
                let Some(comp_account) =
                    comp.account.resolved_account_shared_data(bank)
                else {
                    return Err(
                        ChainlinkError::ResolvedCompanionAccountCouldNoLongerBeFound(
                            companion_pubkey,
                        ),
                    );
                };
                let Some(account) =
                    acc.account.resolved_account_shared_data(bank)
                else {
                    return Err(
                        ChainlinkError::ResolvedAccountCouldNoLongerBeFound(
                            pubkey,
                        ),
                    );
                };
                Ok(AccountWithCompanion {
                    pubkey,
                    account,
                    companion_pubkey,
                    companion_account: Some(comp_account),
                })
            }
        }
    }

    /// Check if an account is currently being watched (subscribed to) by the
    /// remote account provider
    pub fn is_watching(&self, pubkey: &Pubkey) -> bool {
        self.remote_account_provider.is_watching(pubkey)
    }

    /// Subscribe to updates for a specific account
    /// This is typically used when an account is about to be undelegated
    /// and we need to start watching for changes
    #[instrument(skip(self))]
    pub async fn subscribe_to_account(
        &self,
        pubkey: &Pubkey,
    ) -> ChainlinkResult<()> {
        trace!(pubkey = %pubkey, "Subscribing to account");

        self.remote_account_provider
            .subscribe(pubkey)
            .await
            .map_err(|err| {
                ChainlinkError::FailedToSubscribeToAccount(*pubkey, err)
            })
    }

    pub fn chain_slot(&self) -> u64 {
        self.remote_account_provider.chain_slot()
    }

    pub fn received_updates_count(&self) -> u64 {
        self.remote_account_provider.received_updates_count()
    }

    pub(crate) fn promote_accounts(&self, pubkeys: &[&Pubkey]) {
        self.remote_account_provider.promote_accounts(pubkeys);
    }

    pub fn try_get_removed_account_rx(
        &self,
    ) -> ChainlinkResult<mpsc::Receiver<Pubkey>> {
        Ok(self.remote_account_provider.try_get_removed_account_rx()?)
    }

    /// Best-effort airdrop helper: if the account doesn't exist in the bank or has 0 lamports,
    /// create/overwrite it as a plain system account with the provided lamports using the cloner path.
    #[instrument(skip(self))]
    pub async fn airdrop_account_if_empty(
        &self,
        pubkey: Pubkey,
        lamports: u64,
    ) -> ClonerResult<()> {
        if lamports == 0 {
            return Ok(());
        }
        if let Some(acc) = self.accounts_bank.get_account(&pubkey) {
            if acc.lamports() > 0 {
                return Ok(());
            }
        }
        // Build a plain system account with the requested balance
        let account =
            AccountSharedData::new(lamports, 0, &system_program::id());
        debug!(
            pubkey = %pubkey,
            lamports,
            "Auto-airdropping account"
        );
        let _sig = self
            .cloner
            .clone_account(AccountCloneRequest {
                pubkey,
                account,
                commit_frequency_ms: None,
                delegated_to_other: None,
            })
            .await?;
        Ok(())
    }
}

// -----------------
// Tests
// -----------------
#[cfg(test)]
mod tests;
