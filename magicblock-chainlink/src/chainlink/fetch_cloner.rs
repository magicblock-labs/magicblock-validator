use std::{
    collections::{HashMap, HashSet},
    fmt,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};

use dlp::{
    pda::delegation_record_pda_from_delegated_account, state::DelegationRecord,
};
use log::*;
use magicblock_core::traits::AccountsBank;
use solana_account::{AccountSharedData, ReadableAccount};
use solana_pubkey::Pubkey;
use solana_sdk::system_program;
use tokio::{
    sync::{mpsc, oneshot},
    task,
    task::JoinSet,
};

use super::errors::{ChainlinkError, ChainlinkResult};
use crate::{
    chainlink::{
        blacklisted_accounts::blacklisted_accounts,
        should_override_undelegating_account::should_override_undelegating_account,
    },
    cloner::{errors::ClonerResult, Cloner},
    remote_account_provider::{
        program_account::{
            get_loaderv3_get_program_data_address, ProgramAccountResolver,
            LOADER_V1, LOADER_V3,
        },
        ChainPubsubClient, ChainRpcClient, ForwardedSubscriptionUpdate,
        MatchSlotsConfig, RemoteAccount, RemoteAccountProvider,
        ResolvedAccount, ResolvedAccountSharedData,
    },
};

type RemoteAccountRequests = Vec<oneshot::Sender<()>>;

#[derive(Clone)]
pub struct FetchCloner<T, U, V, C>
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
    V: AccountsBank,
    C: Cloner,
{
    /// The RemoteAccountProvider to fetch accounts from
    remote_account_provider: Arc<RemoteAccountProvider<T, U>>,
    /// Tracks pending account fetch requests to avoid duplicate fetches in parallel
    /// Once an account is fetched and cloned into the bank, it's removed from here
    pending_requests: Arc<Mutex<HashMap<Pubkey, RemoteAccountRequests>>>,
    /// Counter to track the number of fetch operations for testing deduplication
    fetch_count: Arc<AtomicU64>,

    accounts_bank: Arc<V>,
    cloner: Arc<C>,
    validator_pubkey: Pubkey,

    /// These are accounts that we should never clone into our validator.
    /// native programs, sysvars, native tokens, validator identity and faucet
    blacklisted_accounts: HashSet<Pubkey>,
}

struct AccountWithCompanion {
    pubkey: Pubkey,
    account: ResolvedAccountSharedData,
    companion_pubkey: Pubkey,
    companion_account: Option<ResolvedAccountSharedData>,
}

#[derive(Debug, Default)]
pub struct FetchAndCloneResult {
    pub not_found_on_chain: Vec<(Pubkey, u64)>,
    pub missing_delegation_record: Vec<(Pubkey, u64)>,
}

impl FetchAndCloneResult {
    pub fn pubkeys_not_found_on_chain(&self) -> Vec<Pubkey> {
        self.not_found_on_chain.iter().map(|(p, _)| *p).collect()
    }

    pub fn pubkeys_missing_delegation_record(&self) -> Vec<Pubkey> {
        self.missing_delegation_record
            .iter()
            .map(|(p, _)| *p)
            .collect()
    }

    pub fn is_ok(&self) -> bool {
        self.not_found_on_chain.is_empty()
            && self.missing_delegation_record.is_empty()
    }
}

impl fmt::Display for FetchAndCloneResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_ok() {
            write!(f, "All accounts fetched and cloned successfully")
        } else {
            if !self.not_found_on_chain.is_empty() {
                writeln!(
                    f,
                    "Accounts not found on chain: {:?}",
                    self.not_found_on_chain
                        .iter()
                        .map(|(p, _)| p.to_string())
                        .collect::<Vec<_>>()
                )?;
            }
            if !self.missing_delegation_record.is_empty() {
                writeln!(
                    f,
                    "Accounts missing delegation record: {:?}",
                    self.missing_delegation_record
                        .iter()
                        .map(|(p, _)| p.to_string())
                        .collect::<Vec<_>>()
                )?;
            }
            Ok(())
        }
    }
}

impl<T, U, V, C> FetchCloner<T, U, V, C>
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
    V: AccountsBank,
    C: Cloner,
{
    /// Create FetchCloner with subscription updates properly connected
    pub fn new(
        remote_account_provider: &Arc<RemoteAccountProvider<T, U>>,
        accounts_bank: &Arc<V>,
        cloner: &Arc<C>,
        validator_pubkey: Pubkey,
        faucet_pubkey: Pubkey,
        subscription_updates_rx: mpsc::Receiver<ForwardedSubscriptionUpdate>,
    ) -> Arc<Self> {
        let blacklisted_accounts =
            blacklisted_accounts(&validator_pubkey, &faucet_pubkey);
        let me = Arc::new(Self {
            remote_account_provider: remote_account_provider.clone(),
            accounts_bank: accounts_bank.clone(),
            cloner: cloner.clone(),
            validator_pubkey,
            pending_requests: Arc::new(Mutex::new(HashMap::new())),
            fetch_count: Arc::new(AtomicU64::new(0)),
            blacklisted_accounts,
        });

        me.clone()
            .start_subscription_listener(subscription_updates_rx);

        me
    }

    /// Get the current fetch count
    pub fn fetch_count(&self) -> u64 {
        self.fetch_count.load(Ordering::Relaxed)
    }

    /// Start listening to subscription updates
    pub fn start_subscription_listener(
        self: Arc<Self>,
        mut subscription_updates: mpsc::Receiver<ForwardedSubscriptionUpdate>,
    ) {
        tokio::spawn(async move {
            while let Some(update) = subscription_updates.recv().await {
                trace!("FetchCloner received subscription update for {} at slot {}",
                    update.pubkey, update.account.slot());
                let pubkey = update.pubkey;

                // TODO: if we get a lot of subs and cannot keep up we need to put this
                // on a separate task so the fetches of delegation records can happen in
                // parallel
                let (resolved_account, deleg_record) =
                    self.resolve_account_to_clone_from_forwarded_sub_with_unsubscribe(update)
                    .await;
                if let Some(account) = resolved_account {
                    // Ensure that the subscription update isn't out of order, i.e. we don't already
                    // hold a newer version of the account in our bank
                    let out_of_order_slot = self
                        .accounts_bank
                        .get_account(&pubkey)
                        .and_then(|in_bank| {
                            if in_bank.remote_slot() >= account.remote_slot() {
                                Some(in_bank.remote_slot())
                            } else {
                                None
                            }
                        });
                    if let Some(in_bank_slot) = out_of_order_slot {
                        warn!(
                            "Ignoring out-of-order subscription update for {pubkey}: bank slot {in_bank_slot}, update slot {}",
                            account.remote_slot()
                        );
                        continue;
                    }

                    if let Some(in_bank) =
                        self.accounts_bank.get_account(&pubkey)
                    {
                        if in_bank.undelegating()
                            && !should_override_undelegating_account(
                                &pubkey,
                                in_bank.delegated(),
                                in_bank.remote_slot(),
                                deleg_record,
                            )
                        {
                            continue;
                        }
                    } else {
                        warn!(
                            "Received update for {pubkey} which is not in bank"
                        );
                    }

                    // Once we clone an account that is delegated to us we no longer need
                    // to receive updates for it from chain
                    // The subscription will be turned back on once the committor service schedules
                    // a commit for it that includes undelegation
                    if account.delegated() {
                        if let Err(err) = self
                            .remote_account_provider
                            .unsubscribe(&pubkey)
                            .await
                        {
                            error!(
                                "Failed to unsubscribe from delegated account {pubkey}: {err}"
                            );
                        }
                    }

                    if account.executable() {
                        self.handle_executable_sub_update(pubkey, account)
                            .await;
                    } else if let Err(err) =
                        self.cloner.clone_account(pubkey, account).await
                    {
                        error!(
                            "Failed to clone account {pubkey} into bank: {err}"
                        );
                    }
                }
            }
        });
    }

    async fn handle_executable_sub_update(
        &self,
        pubkey: Pubkey,
        account: AccountSharedData,
    ) {
        if account.owner().eq(&LOADER_V1) {
            // This is a program deployed on chain with BPFLoader1111111111111111111111111111111111.
            // By definition it cannot be upgraded, hence we should never get a subscription
            // update for it.
            error!("Unexpected subscription update for program to loaded on chain with LoaderV1: {pubkey}.");
            return;
        }

        // For LoaderV3 programs we need to fetch the program data account
        let (program_account, program_data_account) = if account
            .owner()
            .eq(&LOADER_V3)
        {
            match Self::task_to_fetch_with_program_data(
                self,
                pubkey,
                account.remote_slot(),
            )
            .await
            {
                Ok(Ok(account_with_companion)) => (
                    account_with_companion.account.into_account_shared_data(),
                    account_with_companion
                        .companion_account
                        .map(|x| x.into_account_shared_data()),
                ),
                Ok(Err(err)) => {
                    error!(
                        "Failed to fetch program data account for program {pubkey}: {err}."
                    );
                    return;
                }
                Err(err) => {
                    error!(
                        "Failed to fetch program data account for program {pubkey}: {err}."
                    );
                    return;
                }
            }
        } else {
            (account, None::<AccountSharedData>)
        };

        let loaded_program = match ProgramAccountResolver::try_new(
            pubkey,
            *program_account.owner(),
            Some(program_account),
            program_data_account,
        ) {
            Ok(x) => x.into_loaded_program(),
            Err(err) => {
                error!("Failed to resolve program account {pubkey} into bank: {err}");
                return;
            }
        };
        if let Err(err) = self.cloner.clone_program(loaded_program).await {
            error!("Failed to clone account {pubkey} into bank: {err}");
        }
    }

    async fn resolve_account_to_clone_from_forwarded_sub_with_unsubscribe(
        &self,
        update: ForwardedSubscriptionUpdate,
    ) -> (Option<AccountSharedData>, Option<DelegationRecord>) {
        let ForwardedSubscriptionUpdate { pubkey, account } = update;
        let owned_by_delegation_program =
            account.is_owned_by_delegation_program();

        if let Some(account) = account.fresh_account() {
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
                                        error!("Failed to parse delegation record for {pubkey}: {err}. Not cloning account.");
                                        None
                                    }
                                };

                            // If the delegation record is valid we set the owner and delegation
                            // status on the account
                            if let Some(delegation_record) = delegation_record {
                                if log::log_enabled!(log::Level::Trace) {
                                    trace!("Delegation record found for {pubkey}: {delegation_record:?}");
                                    trace!(
                                        "Cloning delegated account: {pubkey} (remote slot {}, owner: {})",
                                        account.remote_slot(),
                                        delegation_record.owner
                                    );
                                }
                                let is_delegated_to_us = delegation_record
                                    .authority
                                    .eq(&self.validator_pubkey) ||
                                    // TODO(thlorenz): @ once the delegation program supports
                                    // delegating to specific authority we need to remove the below
                                    delegation_record.authority.eq(&Pubkey::default());

                                account
                                    .set_owner(delegation_record.owner)
                                    .set_delegated(is_delegated_to_us);

                                // For accounts delegated to us, always unsubscribe from the delegated account
                                if is_delegated_to_us {
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
                        error!("failed to fetch delegation record for {pubkey}: {err}. not cloning account.");
                        (None, None)
                    }
                    Err(err) => {
                        error!("failed to fetch delegation record for {pubkey}: {err}. not cloning account.");
                        (None, None)
                    }
                }
            } else {
                // Accounts not owned by the delegation program can be cloned as is
                // No unsubscription needed for undelegated accounts
                (Some(account), None)
            }
        } else {
            // This should not happen since we call this method with sub updates which always hold
            // a fresh remote account
            error!("BUG: Received subscription update for {pubkey} without fresh account: {account:?}");
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
        DelegationRecord::try_from_bytes_with_discriminator(data)
            .copied()
            .map_err(|err| {
                ChainlinkError::InvalidDelegationRecord(
                    delegation_record_pubkey,
                    err,
                )
            })
    }

    /// Fetches and parses the delegation record for an account, returning the
    /// parsed DelegationRecord if found and valid, None otherwise.
    async fn fetch_and_parse_delegation_record(
        &self,
        account_pubkey: Pubkey,
        min_context_slot: u64,
    ) -> Option<DelegationRecord> {
        let delegation_record_pubkey =
            delegation_record_pda_from_delegated_account(&account_pubkey);

        match self
            .remote_account_provider
            .try_get_multi_until_slots_match(
                &[delegation_record_pubkey],
                Some(MatchSlotsConfig {
                    min_context_slot: Some(min_context_slot),
                    ..Default::default()
                }),
            )
            .await
        {
            Ok(mut delegation_records) => {
                if let Some(delegation_record_remote) = delegation_records.pop()
                {
                    match delegation_record_remote.fresh_account() {
                        Some(delegation_record_account) => {
                            Self::parse_delegation_record(
                                delegation_record_account.data(),
                                delegation_record_pubkey,
                            )
                            .ok()
                        }
                        None => None,
                    }
                } else {
                    None
                }
            }
            Err(_) => None,
        }
    }

    /// Tries to fetch all accounts in `pubkeys` and clone them into the bank.
    /// If `mark_empty` is provided, accounts in that list that are
    /// not found on chain will be added with zero lamports to the bank.
    ///
    /// - **pubkeys**: list of accounts to fetch and clone
    /// - **mark_empty**: optional list of accounts that should be added as empty if not found on
    ///   chain
    /// - **slot**: optional slot to use as minimum context slot for the accounts being cloned
    async fn fetch_and_clone_accounts(
        &self,
        pubkeys: &[Pubkey],
        mark_empty_if_not_found: Option<&[Pubkey]>,
        slot: Option<u64>,
    ) -> ChainlinkResult<FetchAndCloneResult> {
        if log::log_enabled!(log::Level::Trace) {
            let pubkeys = pubkeys
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join(", ");

            trace!("Fetching and cloning accounts: {pubkeys}");
        }

        // We keep all existing subscriptions including delegation records and program data
        // accounts that were directly requested
        let delegation_records = pubkeys
            .iter()
            .map(delegation_record_pda_from_delegated_account)
            .collect::<HashSet<_>>();
        let program_data_accounts = pubkeys
            .iter()
            .map(get_loaderv3_get_program_data_address)
            .collect::<HashSet<_>>();
        let existing_subs: HashSet<&Pubkey> = pubkeys
            .iter()
            .chain(delegation_records.iter())
            .chain(program_data_accounts.iter())
            .filter(|x| self.is_watching(x))
            .collect();

        // Increment fetch counter for testing deduplication (count per account being fetched)
        self.fetch_count
            .fetch_add(pubkeys.len() as u64, Ordering::Relaxed);

        let accs = self
            .remote_account_provider
            .try_get_multi(pubkeys, mark_empty_if_not_found)
            .await?;

        trace!("Fetched {accs:?}");

        let (not_found, mut in_bank, plain, owned_by_deleg, programs) =
            accs.into_iter().zip(pubkeys).fold(
                (vec![], vec![], vec![], vec![], vec![]),
                |(
                    mut not_found,
                    mut in_bank,
                    mut plain,
                    mut owned_by_deleg,
                    mut programs,
                ),
                 (acc, &pubkey)| {
                    use RemoteAccount::*;
                    match acc {
                        NotFound(slot) => not_found.push((pubkey, slot)),
                        Found(remote_account_state) => {
                            match remote_account_state.account {
                                ResolvedAccount::Fresh(account_shared_data) => {
                                    let slot =
                                        account_shared_data.remote_slot();
                                    if account_shared_data
                                        .owner()
                                        .eq(&dlp::id())
                                    {
                                        owned_by_deleg.push((
                                            pubkey,
                                            account_shared_data,
                                            slot,
                                        ));
                                    } else if account_shared_data.executable() {
                                        // We don't clone native loader programs.
                                        // They should not pass the blacklist in the first place,
                                        // but in case a new native program is introduced we don't want
                                        // to fail
                                        if !account_shared_data
                                            .owner()
                                            .eq(&solana_sdk::native_loader::id(
                                            ))
                                        {
                                            programs.push((
                                                pubkey,
                                                account_shared_data,
                                                slot,
                                            ));
                                        } else {
                                            warn!(
                                                "Not cloning native loader program account: {pubkey} (should have been blacklisted)",
                                            );
                                        }
                                    } else {
                                        plain.push((
                                            pubkey,
                                            account_shared_data,
                                        ));
                                    }
                                }
                                ResolvedAccount::Bank(pubkey) => {
                                    in_bank.push(pubkey);
                                }
                            };
                        }
                    }
                    (not_found, in_bank, plain, owned_by_deleg, programs)
                },
            );

        if log::log_enabled!(log::Level::Trace) {
            let not_found = not_found
                .iter()
                .map(|(pubkey, slot)| (pubkey.to_string(), *slot))
                .collect::<Vec<_>>();
            let in_bank = in_bank
                .iter()
                .map(|(p, _)| p.to_string())
                .collect::<Vec<_>>();
            let plain =
                plain.iter().map(|(p, _)| p.to_string()).collect::<Vec<_>>();
            let owned_by_deleg = owned_by_deleg
                .iter()
                .map(|(pubkey, _, slot)| (pubkey.to_string(), *slot))
                .collect::<Vec<_>>();
            let programs = programs
                .iter()
                .map(|(p, _, _)| p.to_string())
                .collect::<Vec<_>>();
            trace!(
                "Fetched accounts: \nnot_found:      {not_found:?} \nin_bank:        {in_bank:?} \nplain:          {plain:?} \nowned_by_deleg: {owned_by_deleg:?}\nprograms:       {programs:?}",
            );
        }

        let (clone_as_empty, not_found) =
            if let Some(mark_empty) = mark_empty_if_not_found {
                not_found
                    .into_iter()
                    .partition::<Vec<_>, _>(|(p, _)| mark_empty.contains(p))
            } else {
                (vec![], not_found)
            };

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

        // For accounts already in bank we don't need to do anything
        if log::log_enabled!(log::Level::Trace) {
            trace!(
                "Accounts already in bank: {:?}",
                in_bank
                    .iter()
                    .map(|(p, _)| p.to_string())
                    .collect::<Vec<_>>()
            );
        }

        // We mark some accounts as empty if we know that they will never exist on chain
        if log::log_enabled!(log::Level::Trace) && !clone_as_empty.is_empty() {
            trace!(
                "Cloning accounts as empty: {:?}",
                clone_as_empty
                    .iter()
                    .map(|(p, _)| p.to_string())
                    .collect::<Vec<_>>()
            );
        }

        // For accounts in the bank that are marked as undelegating, check if they're still
        // delegated to us. If not, we need to refetch them from chain instead of using the
        // bank version.
        let mut accounts_to_refetch = vec![];
        for (pubkey, slot) in &in_bank {
            if let Some(in_bank) = self.accounts_bank.get_account(pubkey) {
                if in_bank.undelegating() {
                    let deleg_record = self
                        .fetch_and_parse_delegation_record(
                            *pubkey,
                            *slot.max(
                                &self.remote_account_provider.chain_slot(),
                            ),
                        )
                        .await;
                    if should_override_undelegating_account(
                        pubkey,
                        in_bank.delegated(),
                        *slot,
                        deleg_record,
                    ) {
                        debug!(
                            "Account {pubkey} marked as undelegating will be overridden since undelegation completed"
                        );
                        accounts_to_refetch.push((*pubkey, *slot));
                        continue;
                    }
                }
            }
        }

        // Remove accounts that need to be refetched from in_bank list
        in_bank.retain(|(pubkey, _)| {
            !accounts_to_refetch.iter().any(|(p, _)| p == pubkey)
        });

        // Add accounts that need to be refetched to the plain list
        // (they will be fetched from chain)
        let mut plain = plain;
        let mut owned_by_deleg = owned_by_deleg;
        for (pubkey, _slot) in accounts_to_refetch {
            if let Some(account) = self
                .remote_account_provider
                .try_get(pubkey)
                .await?
                .fresh_account()
            {
                if account.owner().eq(&dlp::id()) {
                    let remote_slot = account.remote_slot();
                    owned_by_deleg.push((pubkey, account, remote_slot));
                } else {
                    plain.push((pubkey, account));
                }
            }
        }

        // Calculate min context slot: use the greater of subscription slot or last chain slot
        let min_context_slot = slot.map(|subscription_slot| {
            subscription_slot.max(self.remote_account_provider.chain_slot())
        });

        // For potentially delegated accounts we update the owner and delegation state first
        let mut fetch_with_delegation_record_join_set = JoinSet::new();
        for (pubkey, _, account_slot) in &owned_by_deleg {
            let effective_slot = if let Some(min_slot) = min_context_slot {
                min_slot.max(*account_slot)
            } else {
                *account_slot
            };
            fetch_with_delegation_record_join_set.spawn(
                self.task_to_fetch_with_delegation_record(
                    *pubkey,
                    effective_slot,
                ),
            );
        }

        let mut missing_delegation_record = vec![];

        // We remove all new subs for accounts that were not found or already in the bank
        let (accounts_to_clone, record_subs) = {
            let joined = fetch_with_delegation_record_join_set.join_all().await;
            let (errors, accounts_fully_resolved) = joined.into_iter().fold(
                (vec![], vec![]),
                |(mut errors, mut successes), res| {
                    match res {
                        Ok(Ok(account_with_deleg)) => {
                            successes.push(account_with_deleg)
                        }
                        Ok(Err(err)) => errors.push(err),
                        Err(err) => errors.push(err.into()),
                    }
                    (errors, successes)
                },
            );

            // If we encounter any error while fetching delegated accounts then
            // we have to abort as we cannot resume without the ability to sync
            // with the remote
            if !errors.is_empty() {
                // Cancel all new subs since we won't clone any accounts
                cancel_subs(
                    &self.remote_account_provider,
                    CancelStrategy::New {
                        new_subs: pubkeys.iter().cloned().collect(),
                        existing_subs: existing_subs
                            .into_iter()
                            .cloned()
                            .collect(),
                    },
                )
                .await;
                return Err(ChainlinkError::DelegatedAccountResolutionsFailed(
                    errors
                        .iter()
                        .map(|e| e.to_string())
                        .collect::<Vec<_>>()
                        .join(", "),
                ));
            }

            // Cancel new delegation record subs
            let mut record_subs =
                Vec::with_capacity(accounts_fully_resolved.len());
            let mut accounts_to_clone = plain;

            // Now process the accounts (this can fail without affecting unsubscription)
            for AccountWithCompanion {
                pubkey,
                mut account,
                companion_pubkey: delegation_record_pubkey,
                companion_account: delegation_record,
            } in accounts_fully_resolved.into_iter()
            {
                record_subs.push(delegation_record_pubkey);

                // If the account is delegated we set the owner and delegation state
                if let Some(delegation_record_data) = delegation_record {
                    // NOTE: failing here is fine when resolving all accounts for a transaction
                    // since if something is off we better not run it anyways
                    // However we may consider a different behavior when user is getting
                    // mutliple accounts.
                    let delegation_record = match Self::parse_delegation_record(
                        delegation_record_data.data(),
                        delegation_record_pubkey,
                    ) {
                        Ok(x) => x,
                        Err(err) => {
                            // Cancel all new subs since we won't clone any accounts
                            cancel_subs(
                                &self.remote_account_provider,
                                CancelStrategy::New {
                                    new_subs: pubkeys
                                        .iter()
                                        .cloned()
                                        .chain(record_subs.iter().cloned())
                                        .collect(),
                                    existing_subs: existing_subs
                                        .into_iter()
                                        .cloned()
                                        .collect(),
                                },
                            )
                            .await;
                            return Err(err);
                        }
                    };

                    trace!("Delegation record found for {pubkey}: {delegation_record:?}");
                    let is_delegated_to_us = delegation_record
                        .authority
                        .eq(&self.validator_pubkey) ||
                        // TODO(thlorenz): @ once the delegation program supports
                        // delegating to specific authority we need to remove the below
                        delegation_record.authority.eq(&Pubkey::default());
                    account
                        .set_owner(delegation_record.owner)
                        .set_delegated(is_delegated_to_us);
                } else {
                    missing_delegation_record
                        .push((pubkey, account.remote_slot()));
                }
                accounts_to_clone
                    .push((pubkey, account.into_account_shared_data()));
            }

            (accounts_to_clone, record_subs)
        };

        let (loaded_programs, program_data_subs, errors) = {
            // For LoaderV3 accounts we fetch the program data account
            let mut fetch_with_program_data_join_set = JoinSet::new();
            let (loaderv3_programs, single_account_programs): (Vec<_>, Vec<_>) =
                programs
                    .into_iter()
                    .partition(|(_, acc, _)| acc.owner().eq(&LOADER_V3));

            for (pubkey, _, account_slot) in &loaderv3_programs {
                let effective_slot = if let Some(min_slot) = min_context_slot {
                    min_slot.max(*account_slot)
                } else {
                    *account_slot
                };
                fetch_with_program_data_join_set.spawn(
                    self.task_to_fetch_with_program_data(
                        *pubkey,
                        effective_slot,
                    ),
                );
            }
            let joined = fetch_with_program_data_join_set.join_all().await;
            let (mut errors, accounts_with_program_data) = joined
                .into_iter()
                .fold((vec![], vec![]), |(mut errors, mut successes), res| {
                    match res {
                        Ok(Ok(account_with_program_data)) => {
                            successes.push(account_with_program_data)
                        }
                        Ok(Err(err)) => errors.push(err),
                        Err(err) => errors.push(err.into()),
                    }
                    (errors, successes)
                });
            let mut loaded_programs = vec![];

            // Cancel subs for program data accounts
            let program_data_subs = accounts_with_program_data
                .iter()
                .map(|a| a.companion_pubkey)
                .collect::<HashSet<_>>();

            for AccountWithCompanion {
                pubkey: program_id,
                account: program_account,
                companion_pubkey: program_data_pubkey,
                companion_account: program_data,
            } in accounts_with_program_data.into_iter()
            {
                if let Some(program_data) = program_data {
                    let owner = *program_account.owner();
                    let program_data_account =
                        program_data.into_account_shared_data();
                    let loaded_program = ProgramAccountResolver::try_new(
                        program_id,
                        owner,
                        None,
                        Some(program_data_account),
                    )?
                    .into_loaded_program();
                    loaded_programs.push(loaded_program);
                } else {
                    errors.push(
                        ChainlinkError::FailedToResolveProgramDataAccount(
                            program_data_pubkey,
                            program_id,
                        ),
                    );
                }
            }
            for (program_id, program_account, _) in single_account_programs {
                let owner = *program_account.owner();
                let loaded_program = ProgramAccountResolver::try_new(
                    program_id,
                    owner,
                    Some(program_account),
                    None,
                )?
                .into_loaded_program();
                loaded_programs.push(loaded_program);
            }
            (loaded_programs, program_data_subs, errors)
        };
        if !errors.is_empty() {
            // Cancel all new subs since we won't clone any accounts
            cancel_subs(
                &self.remote_account_provider,
                CancelStrategy::New {
                    new_subs: pubkeys
                        .iter()
                        .cloned()
                        .chain(program_data_subs.iter().cloned())
                        .collect(),
                    existing_subs: existing_subs.into_iter().cloned().collect(),
                },
            )
            .await;
            return Err(ChainlinkError::ProgramAccountResolutionsFailed(
                errors
                    .iter()
                    .map(|e| e.to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
            ));
        }

        // Cancel new subs for accounts we don't clone
        let acc_subs = pubkeys.iter().filter(|pubkey| {
            !accounts_to_clone.iter().any(|(p, _)| p.eq(pubkey))
                && !loaded_programs.iter().any(|p| p.program_id.eq(pubkey))
        });

        // Cancel subs for delegated accounts (accounts we clone but don't need to watch)
        let delegated_acc_subs: HashSet<Pubkey> = accounts_to_clone
            .iter()
            .filter_map(|(pubkey, account)| {
                if account.delegated() {
                    Some(*pubkey)
                } else {
                    None
                }
            })
            .collect();

        // Handle sub cancelation now since we may potentially fail during a cloning step
        cancel_subs(
            &self.remote_account_provider,
            CancelStrategy::Hybrid {
                new_subs: record_subs
                    .iter()
                    .cloned()
                    .chain(acc_subs.into_iter().cloned().collect::<Vec<_>>())
                    .chain(program_data_subs.into_iter())
                    .collect::<HashSet<_>>(),
                existing_subs: existing_subs.into_iter().cloned().collect(),
                all: delegated_acc_subs,
            },
        )
        .await;

        let mut join_set = JoinSet::new();
        for acc in accounts_to_clone {
            let (pubkey, account) = acc;
            if log::log_enabled!(log::Level::Trace) {
                trace!(
                    "Cloning account: {pubkey} (remote slot {}, owner: {})",
                    account.remote_slot(),
                    account.owner()
                );
            };

            let cloner = self.cloner.clone();
            join_set.spawn(async move {
                cloner.clone_account(pubkey, account).await
            });
        }

        for acc in loaded_programs {
            let cloner = self.cloner.clone();
            join_set.spawn(async move { cloner.clone_program(acc).await });
        }

        join_set
            .join_all()
            .await
            .into_iter()
            .collect::<ClonerResult<Vec<_>>>()?;

        Ok(FetchAndCloneResult {
            not_found_on_chain: not_found,
            missing_delegation_record,
        })
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
    pub async fn fetch_and_clone_accounts_with_dedup(
        &self,
        pubkeys: &[Pubkey],
        mark_empty_if_not_found: Option<&[Pubkey]>,
        slot: Option<u64>,
    ) -> ChainlinkResult<FetchAndCloneResult> {
        // We cannot clone blacklisted accounts, thus either they are already
        // in the bank (e.g. native programs) or they don't exist and the transaction
        // will fail later
        let pubkeys = pubkeys
            .iter()
            .filter(|p| !self.blacklisted_accounts.contains(p))
            .collect::<Vec<_>>();
        if log::log_enabled!(log::Level::Trace) {
            let pubkeys_str = pubkeys
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            trace!("Fetching and cloning accounts with dedup: {pubkeys_str}");
        }

        let mut await_pending = vec![];
        let mut fetch_new = vec![];

        // Check pending requests and bank synchronously
        {
            let mut pending = self
                .pending_requests
                .lock()
                .expect("pending_requests lock poisoned");

            for pubkey in pubkeys {
                // Check synchronously if account is in bank and subscribed when it should be
                if let Some(account_in_bank) =
                    self.accounts_bank.get_account(pubkey)
                {
                    // NOTE: we defensively correct accounts that we should have been watching but
                    //       were not for some reason. We fetch them again in that case.
                    //       This actually would point to a bug in the subscription logic.
                    // TODO(thlorenz): remove this once we are certain (by perusing logs) that this
                    //                 does not happen anymore
                    if account_in_bank.owner().eq(&dlp::id())
                        || account_in_bank.delegated()
                        || self.blacklisted_accounts.contains(pubkey)
                        || self.is_watching(pubkey)
                    {
                        continue;
                    } else if !self.is_watching(pubkey) {
                        debug!("Account {pubkey} should be watched but wasn't");
                    }
                }

                // Check if account fetch is already pending
                if let Some(requests) = pending.get_mut(pubkey) {
                    let (sender, receiver) = oneshot::channel();
                    requests.push(sender);
                    await_pending.push((*pubkey, receiver));
                    continue;
                }

                // Account needs to be fetched - add to fetch list
                fetch_new.push(*pubkey);
            }

            // Create pending entries for accounts we need to fetch
            for &pubkey in &fetch_new {
                pending.insert(pubkey, vec![]);
            }
        }

        // If we have accounts to fetch, delegate to the existing implementation
        // but notify all pending requests when done
        let result = if !fetch_new.is_empty() {
            self.fetch_and_clone_accounts(
                &fetch_new,
                mark_empty_if_not_found,
                slot,
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
        {
            let mut pending = self
                .pending_requests
                .lock()
                .expect("pending_requests lock poisoned");
            for &pubkey in &fetch_new {
                if let Some(requests) = pending.remove(&pubkey) {
                    // We signal completion but don't send the actual account data since:
                    // 1. The account is now in the bank if it was successfully cloned
                    // 2. If there was an error, the result will contain the error info
                    // 3. Pending requesters can check the bank or result as needed
                    for sender in requests {
                        let _ = sender.send(());
                    }
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
                        warn!("FetchCloner::clone_accounts - RecvError occurred while awaiting account {}: {err:?}. This indicates the account fetch sender was dropped without sending a value.", pubkey);
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
    ) -> task::JoinHandle<ChainlinkResult<AccountWithCompanion>> {
        let delegation_record_pubkey =
            delegation_record_pda_from_delegated_account(&pubkey);
        self.task_to_fetch_with_companion(
            pubkey,
            delegation_record_pubkey,
            slot,
        )
    }

    fn task_to_fetch_with_program_data(
        &self,
        pubkey: Pubkey,
        slot: u64,
    ) -> task::JoinHandle<ChainlinkResult<AccountWithCompanion>> {
        let program_data_pubkey =
            get_loaderv3_get_program_data_address(&pubkey);
        self.task_to_fetch_with_companion(pubkey, program_data_pubkey, slot)
    }

    fn task_to_fetch_with_companion(
        &self,
        pubkey: Pubkey,
        delegation_record_pubkey: Pubkey,
        slot: u64,
    ) -> task::JoinHandle<ChainlinkResult<AccountWithCompanion>> {
        let provider = self.remote_account_provider.clone();
        let bank = self.accounts_bank.clone();
        let fetch_count = self.fetch_count.clone();
        task::spawn(async move {
            trace!("Fetching account {pubkey} with delegation record {delegation_record_pubkey} at slot {slot}");

            // Increment fetch counter for testing deduplication (2 accounts: pubkey + delegation_record_pubkey)
            fetch_count.fetch_add(2, Ordering::Relaxed);

            provider
                .try_get_multi_until_slots_match(
                    &[pubkey, delegation_record_pubkey],
                    Some(MatchSlotsConfig {
                            min_context_slot: Some(slot),
                            ..Default::default()
                        }),
                )
                .await
                // SAFETY: we always get two results here
                .map(|mut accs| {
                    let acc_last = accs.pop().unwrap();
                    let acc_first = accs.pop().unwrap();
                    (acc_first, acc_last)
                })
                .map_err(ChainlinkError::from)
                .and_then(|(acc, deleg)| {
                    use RemoteAccount::*;
                    match (acc, deleg) {
                        // Account not found even though we found it previously - this is invalid,
                        // either way we cannot use it now
                        (NotFound(_), NotFound(_)) |
                        (NotFound(_), Found(_)) => Err(ChainlinkError::ResolvedAccountCouldNoLongerBeFound(
                                pubkey
                            )),
                        (Found(acc), NotFound(_)) => {
                            // Only account found without a delegation record, it is either invalid
                            // or a delegation record itself.
                            // Clone it as is (without changing the owner or flagging as delegated)
                            match acc.account.resolved_account_shared_data(&*bank) {
                                Some(account) =>
                                    Ok(AccountWithCompanion {
                                        pubkey,
                                        account,
                                        companion_pubkey: delegation_record_pubkey,
                                        companion_account: None,
                                     }),
                                None => Err(
                                    ChainlinkError::ResolvedAccountCouldNoLongerBeFound(
                                        pubkey
                                    ),
                                ),
                            }
                        }
                        (Found(acc), Found(deleg)) => {
                            // Found the delegation record, we include it so that the caller can
                            // use it to add metadata to the account and use it for decision making
                            let Some(deleg_account) =
                                deleg.account.resolved_account_shared_data(&*bank)
                            else {
                                return Err(
                                    ChainlinkError::ResolvedAccountCouldNoLongerBeFound(
                                    pubkey
                                ));
                            };
                            let Some(account) = acc.account.resolved_account_shared_data(&*bank) else {
                                return Err(
                                    ChainlinkError::ResolvedAccountCouldNoLongerBeFound(
                                        pubkey
                                    ),
                                );
                            };
                            Ok(AccountWithCompanion {
                                pubkey,
                                account,
                                companion_pubkey: delegation_record_pubkey,
                                companion_account: Some(deleg_account),
                             })
                        },
                    }
                })
        })
    }

    /// Check if an account is currently being watched (subscribed to) by the
    /// remote account provider
    pub fn is_watching(&self, pubkey: &Pubkey) -> bool {
        self.remote_account_provider.is_watching(pubkey)
    }

    /// Subscribe to updates for a specific account
    /// This is typically used when an account is about to be undelegated
    /// and we need to start watching for changes
    pub async fn subscribe_to_account(
        &self,
        pubkey: &Pubkey,
    ) -> ChainlinkResult<()> {
        trace!("Subscribing to account: {pubkey}");

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
            "Auto-airdropping {} lamports to new/empty account {}",
            lamports, pubkey
        );
        let _sig = self.cloner.clone_account(pubkey, account).await?;
        Ok(())
    }
}

// -----------------
// Helpers
// -----------------
enum CancelStrategy {
    /// Cancel all subscriptions for the given pubkeys
    All(HashSet<Pubkey>),
    /// Cancel subscriptions for new accounts that are not in existing subscriptions
    New {
        new_subs: HashSet<Pubkey>,
        existing_subs: HashSet<Pubkey>,
    },
    /// Cancel subscriptions for new accounts that are not in existing subscriptions
    /// and also cancel all subscriptions for the given pubkeys in `all`
    Hybrid {
        new_subs: HashSet<Pubkey>,
        existing_subs: HashSet<Pubkey>,
        all: HashSet<Pubkey>,
    },
}

impl CancelStrategy {
    fn is_empty(&self) -> bool {
        match self {
            CancelStrategy::All(pubkeys) => pubkeys.is_empty(),
            CancelStrategy::New {
                new_subs,
                existing_subs,
            } => new_subs.is_empty() && existing_subs.is_empty(),
            CancelStrategy::Hybrid {
                new_subs,
                existing_subs,
                all,
            } => {
                new_subs.is_empty()
                    && existing_subs.is_empty()
                    && all.is_empty()
            }
        }
    }
}

impl fmt::Display for CancelStrategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CancelStrategy::All(pubkeys) => write!(
                f,
                "All({})",
                pubkeys
                    .iter()
                    .map(|p| p.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            CancelStrategy::New {
                new_subs,
                existing_subs,
            } => write!(
                f,
                "New({}) Existing({})",
                new_subs
                    .iter()
                    .map(|p| p.to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
                existing_subs
                    .iter()
                    .map(|p| p.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            CancelStrategy::Hybrid {
                new_subs,
                existing_subs,
                all,
            } => write!(
                f,
                "Hybrid(New: {}, Existing: {}, All: {})",
                new_subs
                    .iter()
                    .map(|p| p.to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
                existing_subs
                    .iter()
                    .map(|p| p.to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
                all.iter()
                    .map(|p| p.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

async fn cancel_subs<T: ChainRpcClient, U: ChainPubsubClient>(
    provider: &Arc<RemoteAccountProvider<T, U>>,
    strategy: CancelStrategy,
) {
    if strategy.is_empty() {
        trace!("No subscriptions to cancel");
        return;
    }
    let mut joinset = JoinSet::new();

    trace!("Canceling subscriptions with strategy: {strategy}");
    let subs_to_cancel = match strategy {
        CancelStrategy::All(pubkeys) => pubkeys,
        CancelStrategy::New {
            new_subs,
            existing_subs,
        } => new_subs.difference(&existing_subs).cloned().collect(),
        CancelStrategy::Hybrid {
            new_subs,
            existing_subs,
            all,
        } => new_subs
            .difference(&existing_subs)
            .cloned()
            .chain(all.into_iter())
            .collect(),
    };
    if log::log_enabled!(log::Level::Trace) {
        trace!(
            "Canceling subscriptions for: {}",
            subs_to_cancel
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    for pubkey in subs_to_cancel {
        let provider_clone = provider.clone();
        joinset.spawn(async move {
            // Check if there are pending requests for this account before unsubscribing
            // This prevents race conditions where one operation unsubscribes while another still needs it
            if provider_clone.is_pending(&pubkey) {
                debug!(
                    "Skipping unsubscribe for {pubkey} - has pending requests"
                );
                return;
            }

            if let Err(err) = provider_clone.unsubscribe(&pubkey).await {
                warn!("Failed to unsubscribe from {pubkey}: {err:?}");
            }
        });
    }

    joinset.join_all().await;
}

// -----------------
// Tests
// -----------------
#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use solana_account::{Account, AccountSharedData, WritableAccount};
    use solana_sdk::system_program;
    use tokio::sync::mpsc;

    use super::*;
    use crate::{
        accounts_bank::mock::AccountsBankStub,
        assert_not_cloned, assert_not_subscribed, assert_subscribed,
        assert_subscribed_without_delegation_record,
        config::LifecycleMode,
        remote_account_provider::{
            chain_pubsub_client::mock::ChainPubsubClientMock,
            config::RemoteAccountProviderConfig, RemoteAccountProvider,
        },
        testing::{
            accounts::{
                account_shared_with_owner, delegated_account_shared_with_owner,
                delegated_account_shared_with_owner_and_slot,
            },
            cloner_stub::ClonerStub,
            deleg::{
                add_delegation_record_for, add_invalid_delegation_record_for,
            },
            init_logger,
            rpc_client_mock::{ChainRpcClientMock, ChainRpcClientMockBuilder},
            utils::random_pubkey,
        },
    };

    type TestFetchClonerResult = (
        Arc<
            FetchCloner<
                ChainRpcClientMock,
                ChainPubsubClientMock,
                AccountsBankStub,
                ClonerStub,
            >,
        >,
        mpsc::Sender<ForwardedSubscriptionUpdate>,
    );

    macro_rules! _cloned_account {
        ($bank:expr,
         $account_pubkey:expr,
         $expected_account:expr,
         $expected_slot:expr,
         $delegated:expr,
         $owner:expr) => {{
            let cloned_account = $bank.get_account(&$account_pubkey);
            assert!(cloned_account.is_some());
            let cloned_account = cloned_account.unwrap();
            let mut expected_account =
                AccountSharedData::from($expected_account);
            expected_account.set_remote_slot($expected_slot);
            expected_account.set_delegated($delegated);
            expected_account.set_owner($owner);

            assert_eq!(cloned_account, expected_account);
            assert_eq!(cloned_account.remote_slot(), $expected_slot);
            cloned_account
        }};
    }

    macro_rules! assert_cloned_delegated_account {
        ($bank:expr, $account_pubkey:expr, $expected_account:expr, $expected_slot:expr, $owner:expr) => {{
            _cloned_account!(
                $bank,
                $account_pubkey,
                $expected_account,
                $expected_slot,
                true,
                $owner
            )
        }};
    }

    macro_rules! assert_cloned_undelegated_account {
        ($bank:expr, $account_pubkey:expr, $expected_account:expr, $expected_slot:expr, $owner:expr) => {{
            _cloned_account!(
                $bank,
                $account_pubkey,
                $expected_account,
                $expected_slot,
                false,
                $owner
            )
        }};
    }

    struct FetcherTestCtx {
        remote_account_provider: Arc<
            RemoteAccountProvider<ChainRpcClientMock, ChainPubsubClientMock>,
        >,
        accounts_bank: Arc<AccountsBankStub>,
        rpc_client: crate::testing::rpc_client_mock::ChainRpcClientMock,
        #[allow(unused)]
        forward_rx: mpsc::Receiver<ForwardedSubscriptionUpdate>,
        fetch_cloner: Arc<
            FetchCloner<
                ChainRpcClientMock,
                ChainPubsubClientMock,
                AccountsBankStub,
                ClonerStub,
            >,
        >,
        #[allow(unused)]
        subscription_tx: mpsc::Sender<ForwardedSubscriptionUpdate>,
    }

    async fn setup<I>(
        accounts: I,
        current_slot: u64,
        validator_pubkey: Pubkey,
    ) -> FetcherTestCtx
    where
        I: IntoIterator<Item = (Pubkey, Account)>,
    {
        init_logger();

        let faucet_pubkey = Pubkey::new_unique();

        // Setup mock RPC client with the accounts and clock sysvar
        let accounts_map: HashMap<Pubkey, Account> =
            accounts.into_iter().collect();
        let rpc_client = ChainRpcClientMockBuilder::new()
            .slot(current_slot)
            .clock_sysvar_for_slot(current_slot)
            .accounts(accounts_map)
            .build();

        // Setup components
        let (updates_sender, updates_receiver) = mpsc::channel(1_000);
        let pubsub_client =
            ChainPubsubClientMock::new(updates_sender, updates_receiver);
        let accounts_bank = Arc::new(AccountsBankStub::default());
        let rpc_client_clone = rpc_client.clone();

        let (forward_tx, forward_rx) = mpsc::channel(1_000);
        let remote_account_provider = Arc::new(
            RemoteAccountProvider::new(
                rpc_client,
                pubsub_client,
                forward_tx,
                &RemoteAccountProviderConfig::try_new_with_metrics(
                    1000,
                    LifecycleMode::Ephemeral,
                    false,
                )
                .unwrap(),
            )
            .await
            .unwrap(),
        );
        let (fetch_cloner, subscription_tx) = init_fetch_cloner(
            remote_account_provider.clone(),
            &accounts_bank,
            validator_pubkey,
            faucet_pubkey,
        );

        FetcherTestCtx {
            remote_account_provider,
            accounts_bank,
            rpc_client: rpc_client_clone,
            forward_rx,
            fetch_cloner,
            subscription_tx,
        }
    }

    /// Helper function to initialize FetchCloner for tests with subscription updates
    /// Returns (FetchCloner, subscription_sender) for simulating subscription updates in tests
    fn init_fetch_cloner(
        remote_account_provider: Arc<
            RemoteAccountProvider<ChainRpcClientMock, ChainPubsubClientMock>,
        >,
        bank: &Arc<AccountsBankStub>,
        validator_pubkey: Pubkey,
        faucet_pubkey: Pubkey,
    ) -> TestFetchClonerResult {
        let (subscription_tx, subscription_rx) = mpsc::channel(100);
        let cloner = Arc::new(ClonerStub::new(bank.clone()));
        let fetch_cloner = FetchCloner::new(
            &remote_account_provider,
            bank,
            &cloner,
            validator_pubkey,
            faucet_pubkey,
            subscription_rx,
        );
        (fetch_cloner, subscription_tx)
    }

    // -----------------
    // Single Account Tests
    // -----------------
    #[tokio::test]
    async fn test_fetch_and_clone_single_non_delegated_account() {
        let validator_pubkey = random_pubkey();
        let account_pubkey = random_pubkey();
        let account_owner = random_pubkey();

        // Create a non-delegated account
        let account = Account {
            lamports: 1_000_000,
            data: vec![1, 2, 3, 4],
            owner: account_owner,
            executable: false,
            rent_epoch: 0,
        };

        let FetcherTestCtx {
            accounts_bank,
            fetch_cloner,
            ..
        } = setup([(account_pubkey, account.clone())], 100, validator_pubkey)
            .await;

        let result = fetch_cloner
            .fetch_and_clone_accounts(&[account_pubkey], None, None)
            .await;

        debug!("Test result: {result:?}");

        assert!(result.is_ok());
        assert_cloned_undelegated_account!(
            accounts_bank,
            account_pubkey,
            account,
            100,
            account_owner
        );
    }

    #[tokio::test]
    async fn test_fetch_and_clone_single_non_existing_account() {
        let validator_pubkey = random_pubkey();
        let non_existing_pubkey = random_pubkey();

        // Setup with no accounts (empty collection)
        let FetcherTestCtx {
            accounts_bank,
            fetch_cloner,
            ..
        } = setup(
            std::iter::empty::<(Pubkey, Account)>(),
            100,
            validator_pubkey,
        )
        .await;

        let result = fetch_cloner
            .fetch_and_clone_accounts(&[non_existing_pubkey], None, None)
            .await;

        debug!("Test result: {result:?}");

        // Verify success (non-existing accounts are handled gracefully)
        assert!(result.is_ok());

        // Verify no account was cloned
        let cloned_account = accounts_bank.get_account(&non_existing_pubkey);
        assert!(cloned_account.is_none());
    }

    #[tokio::test]
    async fn test_fetch_and_clone_single_delegated_account_with_valid_delegation_record(
    ) {
        let validator_pubkey = random_pubkey();
        let account_pubkey = random_pubkey();
        let account_owner = random_pubkey();
        const CURRENT_SLOT: u64 = 100;

        // Create a delegated account (owned by dlp)
        let account = Account {
            lamports: 1_234,
            data: vec![1, 2, 3, 4],
            owner: dlp::id(),
            executable: false,
            rent_epoch: 0,
        };

        // Setup with just the delegated account
        let FetcherTestCtx {
            remote_account_provider,
            accounts_bank,
            rpc_client,
            fetch_cloner,
            ..
        } = setup(
            [(account_pubkey, account.clone())],
            CURRENT_SLOT,
            validator_pubkey,
        )
        .await;

        // Add delegation record
        let deleg_record_pubkey = add_delegation_record_for(
            &rpc_client,
            account_pubkey,
            validator_pubkey,
            account_owner,
        );

        // Test fetch and clone
        let result = fetch_cloner
            .fetch_and_clone_accounts(&[account_pubkey], None, None)
            .await;

        debug!("Test result: {result:?}");

        assert!(result.is_ok());

        // Verify account was cloned with correct delegation properties
        let cloned_account = accounts_bank.get_account(&account_pubkey);
        assert!(cloned_account.is_some());
        let cloned_account = cloned_account.unwrap();

        // The cloned account should have the delegation owner and be marked as delegated
        let mut expected_account =
            delegated_account_shared_with_owner(&account, account_owner);
        expected_account.set_remote_slot(CURRENT_SLOT);
        assert_eq!(cloned_account, expected_account);

        // Assert correct remote_slot
        assert_eq!(cloned_account.remote_slot(), CURRENT_SLOT);

        // Verify delegation record was not cloned (only the delegated account is cloned)
        assert!(accounts_bank.get_account(&deleg_record_pubkey).is_none());

        // Delegated accounts to us should not be subscribed since we control them
        assert_not_subscribed!(
            remote_account_provider,
            &[&account_pubkey, &deleg_record_pubkey]
        );
    }

    #[tokio::test]
    async fn test_fetch_and_clone_single_delegated_account_with_different_authority(
    ) {
        let validator_pubkey = random_pubkey();
        let different_authority = random_pubkey(); // Different authority
        let account_pubkey = random_pubkey();
        let account_owner = random_pubkey();
        const CURRENT_SLOT: u64 = 100;

        // Create a delegated account (owned by dlp)
        let account = Account {
            lamports: 1_234,
            data: vec![1, 2, 3, 4],
            owner: dlp::id(),
            executable: false,
            rent_epoch: 0,
        };

        // Setup with just the delegated account
        let FetcherTestCtx {
            remote_account_provider,
            accounts_bank,
            rpc_client,
            fetch_cloner,
            ..
        } = setup(
            [(account_pubkey, account.clone())],
            CURRENT_SLOT,
            validator_pubkey,
        )
        .await;

        // Add delegation record with a different authority (not our validator)
        let deleg_record_pubkey = add_delegation_record_for(
            &rpc_client,
            account_pubkey,
            different_authority,
            account_owner,
        );

        let result = fetch_cloner
            .fetch_and_clone_accounts(&[account_pubkey], None, None)
            .await;

        debug!("Test result: {result:?}");

        assert!(result.is_ok());

        // Verify account was cloned but NOT marked as delegated since authority is different
        let cloned_account = accounts_bank.get_account(&account_pubkey);
        assert!(cloned_account.is_some());
        let cloned_account = cloned_account.unwrap();

        // The cloned account should have the delegation owner but NOT be marked as delegated
        // since the authority doesn't match our validator
        let mut expected_account =
            account_shared_with_owner(&account, account_owner);
        expected_account.set_remote_slot(CURRENT_SLOT);
        assert_eq!(cloned_account, expected_account);

        // Specifically verify it's not marked as delegated
        assert!(!cloned_account.delegated());

        // Assert correct remote_slot
        assert_eq!(cloned_account.remote_slot(), CURRENT_SLOT);

        // Verify delegation record was not cloned (only the delegated account is cloned)
        assert!(accounts_bank.get_account(&deleg_record_pubkey).is_none());

        assert_subscribed!(remote_account_provider, &[&account_pubkey]);
        assert_not_subscribed!(
            remote_account_provider,
            &[&deleg_record_pubkey]
        );
    }

    #[tokio::test]
    async fn test_fetch_and_clone_single_delegated_account_without_delegation_record_that_has_sub(
    ) {
        // In case the delegation record itself was subscribed to already and then we subscribe to
        // the account itself, then the subscription to the delegation record should not be removed
        let validator_pubkey = random_pubkey();
        let account_pubkey = random_pubkey();
        let account_owner = random_pubkey();

        const CURRENT_SLOT: u64 = 100;

        // Create a delegated account (owned by dlp)
        let account = Account {
            lamports: 1_234,
            data: vec![1, 2, 3, 4],
            owner: dlp::id(),
            executable: false,
            rent_epoch: 0,
        };

        // Setup with just the delegated account
        let FetcherTestCtx {
            remote_account_provider,
            accounts_bank,
            fetch_cloner,
            rpc_client,
            ..
        } = setup(
            [(account_pubkey, account.clone())],
            CURRENT_SLOT,
            validator_pubkey,
        )
        .await;

        // Delegation record is cloned previously
        let deleg_record_pubkey = add_delegation_record_for(
            &rpc_client,
            account_pubkey,
            validator_pubkey,
            account_owner,
        );
        let result = fetch_cloner
            .fetch_and_clone_accounts(&[deleg_record_pubkey], None, None)
            .await;
        assert!(result.is_ok());

        // Verify delegation record was cloned
        assert!(accounts_bank.get_account(&deleg_record_pubkey).is_some());

        // Fetch and clone the delegated account
        let result = fetch_cloner
            .fetch_and_clone_accounts(&[account_pubkey], None, None)
            .await;

        assert!(result.is_ok());

        // Verify account was cloned correctly
        let cloned_account = accounts_bank.get_account(&account_pubkey);
        assert!(cloned_account.is_some());
        let cloned_account = cloned_account.unwrap();

        let expected_account = delegated_account_shared_with_owner_and_slot(
            &account,
            account_owner,
            CURRENT_SLOT,
        );
        assert_eq!(cloned_account, expected_account);

        // Verify delegation record was not removed
        assert!(accounts_bank.get_account(&deleg_record_pubkey).is_some());

        // The subscription to the delegation record should remain
        assert_subscribed!(remote_account_provider, &[&deleg_record_pubkey]);
        // The delegated account should not be subscribed
        assert_not_subscribed!(remote_account_provider, &[&account_pubkey]);
    }

    // -----------------
    // Multi Account Tests
    // -----------------

    #[tokio::test]
    async fn test_fetch_and_clone_multiple_accounts_mixed_types() {
        let validator_pubkey = random_pubkey();
        let account_owner = random_pubkey();
        const CURRENT_SLOT: u64 = 100;

        // Test 1: non-delegated account, delegated account, delegation record
        let non_delegated_pubkey = random_pubkey();
        let delegated_account_pubkey = random_pubkey();
        // This is a delegation record that we are actually cloning into the validator
        let delegation_record_pubkey = random_pubkey();

        let non_delegated_account = Account {
            lamports: 500_000,
            data: vec![10, 20, 30],
            owner: account_owner,
            executable: false,
            rent_epoch: 0,
        };

        let delegated_account = Account {
            lamports: 1_000_000,
            data: vec![1, 2, 3, 4],
            owner: dlp::id(),
            executable: false,
            rent_epoch: 0,
        };

        let delegation_record_account = Account {
            lamports: 2_000_000,
            data: vec![100, 101, 102],
            owner: dlp::id(),
            executable: false,
            rent_epoch: 0,
        };

        let accounts = [
            (non_delegated_pubkey, non_delegated_account.clone()),
            (delegated_account_pubkey, delegated_account.clone()),
            (delegation_record_pubkey, delegation_record_account.clone()),
        ];

        let FetcherTestCtx {
            remote_account_provider,
            accounts_bank,
            rpc_client,
            fetch_cloner,
            ..
        } = setup(accounts, CURRENT_SLOT, validator_pubkey).await;

        // Add delegation record for the delegated account
        add_delegation_record_for(
            &rpc_client,
            delegated_account_pubkey,
            validator_pubkey,
            account_owner,
        );

        let result = fetch_cloner
            .fetch_and_clone_accounts(
                &[
                    non_delegated_pubkey,
                    delegated_account_pubkey,
                    delegation_record_pubkey,
                ],
                None,
                None,
            )
            .await;

        debug!("Test result: {result:?}");

        assert!(result.is_ok());

        assert_cloned_undelegated_account!(
            accounts_bank,
            non_delegated_pubkey,
            non_delegated_account.clone(),
            CURRENT_SLOT,
            non_delegated_account.owner
        );

        assert_cloned_delegated_account!(
            accounts_bank,
            delegated_account_pubkey,
            delegated_account.clone(),
            CURRENT_SLOT,
            account_owner
        );

        // Verify delegation record account was cloned as non-delegated
        // (it's owned by delegation program but has no delegation record itself)
        assert_cloned_undelegated_account!(
            accounts_bank,
            delegation_record_pubkey,
            delegation_record_account,
            CURRENT_SLOT,
            dlp::id()
        );

        assert_subscribed_without_delegation_record!(
            remote_account_provider,
            &[&non_delegated_pubkey, &delegation_record_pubkey]
        );
        assert_not_subscribed!(
            remote_account_provider,
            &[&delegated_account_pubkey]
        );
    }

    #[tokio::test]
    async fn test_fetch_and_clone_valid_delegated_account_and_account_with_invalid_delegation_record(
    ) {
        let validator_pubkey = random_pubkey();
        let account_owner = random_pubkey();
        const CURRENT_SLOT: u64 = 100;

        // Create a delegated account and an account with invalid delegation record
        let delegated_pubkey = random_pubkey();
        let invalid_delegated_pubkey = random_pubkey();

        let delegated_account = Account {
            lamports: 1_000_000,
            data: vec![1, 2, 3, 4],
            owner: dlp::id(),
            executable: false,
            rent_epoch: 0,
        };

        let invalid_delegated_account = Account {
            lamports: 500_000,
            data: vec![5, 6, 7, 8],
            owner: dlp::id(),
            executable: false,
            rent_epoch: 0,
        };

        let accounts = [
            (delegated_pubkey, delegated_account.clone()),
            (invalid_delegated_pubkey, invalid_delegated_account.clone()),
        ];

        let FetcherTestCtx {
            remote_account_provider,
            accounts_bank,
            rpc_client,
            fetch_cloner,
            ..
        } = setup(accounts, CURRENT_SLOT, validator_pubkey).await;

        // Add valid delegation record for first account
        add_delegation_record_for(
            &rpc_client,
            delegated_pubkey,
            validator_pubkey,
            account_owner,
        );

        // Add invalid delegation record for second account
        add_invalid_delegation_record_for(
            &rpc_client,
            invalid_delegated_pubkey,
        );

        let result = fetch_cloner
            .fetch_and_clone_accounts(
                &[delegated_pubkey, invalid_delegated_pubkey],
                None,
                None,
            )
            .await;

        debug!("Test result: {result:?}");

        // Should return an error due to invalid delegation record
        assert!(result.is_err());
        assert!(matches!(
            result,
            Err(ChainlinkError::InvalidDelegationRecord(_, _))
        ));

        // Verify no accounts were cloned nor subscribed due to the error
        assert!(accounts_bank.get_account(&delegated_pubkey).is_none());
        assert!(accounts_bank
            .get_account(&invalid_delegated_pubkey)
            .is_none());

        assert_not_subscribed!(
            remote_account_provider,
            &[&invalid_delegated_pubkey, &delegated_pubkey]
        );
    }

    #[tokio::test]
    async fn test_deleg_record_stale() {
        init_logger();
        let validator_pubkey = random_pubkey();
        let account_owner = random_pubkey();
        const CURRENT_SLOT: u64 = 100;
        const INITIAL_DELEG_RECORD_SLOT: u64 = CURRENT_SLOT - 10;

        // The account to clone is up to date
        let account_pubkey = random_pubkey();
        let account = Account {
            lamports: 1_000_000,
            data: vec![1, 2, 3, 4],
            owner: dlp::id(),
            executable: false,
            rent_epoch: 0,
        };
        let FetcherTestCtx {
            rpc_client,
            fetch_cloner,
            ..
        } = setup(
            [(account_pubkey, account.clone())],
            CURRENT_SLOT,
            validator_pubkey,
        )
        .await;

        // Add delegation record which is stale (10 slots behind)
        let deleg_record_pubkey = add_delegation_record_for(
            &rpc_client,
            account_pubkey,
            validator_pubkey,
            account_owner,
        );
        rpc_client.account_override_slot(
            &deleg_record_pubkey,
            INITIAL_DELEG_RECORD_SLOT,
        );

        // Initially we should not be able to clone the account since we cannot
        // find a valid delegation record (up to date the same way the account is)
        let result = fetch_cloner
            .fetch_and_clone_accounts(&[account_pubkey], None, None)
            .await;

        debug!("Test result: {result:?}");

        // Should return a result indicating missing  delegation record
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap().missing_delegation_record,
            vec![(account_pubkey, CURRENT_SLOT)]
        );

        // After the RPC provider updates the delegation record and has it available
        // at the required slot then all is ok
        rpc_client.account_override_slot(&deleg_record_pubkey, CURRENT_SLOT);
        let result = fetch_cloner
            .fetch_and_clone_accounts(&[account_pubkey], None, None)
            .await;
        debug!("Test result after updating delegation record: {result:?}");
        assert!(result.is_ok());
        assert!(result.unwrap().is_ok());
    }

    #[tokio::test]
    async fn test_account_stale() {
        init_logger();
        let validator_pubkey = random_pubkey();
        let account_owner = random_pubkey();
        const CURRENT_SLOT: u64 = 100;
        const INITIAL_ACC_SLOT: u64 = CURRENT_SLOT - 10;

        // The account to clone starts stale (10 slots behind)
        let account_pubkey = random_pubkey();
        let account = Account {
            lamports: 1_000_000,
            data: vec![1, 2, 3, 4],
            owner: dlp::id(),
            executable: false,
            rent_epoch: 0,
        };
        let FetcherTestCtx {
            rpc_client,
            fetch_cloner,
            ..
        } = setup(
            [(account_pubkey, account.clone())],
            CURRENT_SLOT,
            validator_pubkey,
        )
        .await;

        // Override account slot to make it stale
        rpc_client.account_override_slot(&account_pubkey, INITIAL_ACC_SLOT);

        // Add delegation record which is up to date
        add_delegation_record_for(
            &rpc_client,
            account_pubkey,
            validator_pubkey,
            account_owner,
        );

        // Initially we should not be able to clone the account since the account
        // is stale (delegation record is up to date but account is behind)
        let result = fetch_cloner
            .fetch_and_clone_accounts(&[account_pubkey], None, None)
            .await;

        debug!("Test result: {result:?}");

        // Should return a result indicating the account needs to be updated
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap().not_found_on_chain,
            vec![(account_pubkey, CURRENT_SLOT)]
        );

        // After the RPC provider updates the account to the current slot
        rpc_client.account_override_slot(&account_pubkey, CURRENT_SLOT);
        let result = fetch_cloner
            .fetch_and_clone_accounts(&[account_pubkey], None, None)
            .await;
        debug!("Test result after updating account: {result:?}");
        assert!(result.is_ok());
        assert!(result.unwrap().is_ok());
    }

    #[tokio::test]
    async fn test_delegation_record_unsub_race_condition_prevention() {
        init_logger();
        let validator_pubkey = random_pubkey();
        let account_owner = random_pubkey();
        const CURRENT_SLOT: u64 = 100;

        let account_pubkey = random_pubkey();
        let account = Account {
            lamports: 1_000_000,
            data: vec![1, 2, 3, 4],
            owner: dlp::id(),
            executable: false,
            rent_epoch: 0,
        };

        let FetcherTestCtx {
            remote_account_provider,
            accounts_bank,
            rpc_client,
            fetch_cloner,
            ..
        } = setup(
            [(account_pubkey, account.clone())],
            CURRENT_SLOT,
            validator_pubkey,
        )
        .await;

        // Add delegation record
        let deleg_record_pubkey = add_delegation_record_for(
            &rpc_client,
            account_pubkey,
            validator_pubkey,
            account_owner,
        );

        // Test the race condition prevention:
        // 1. Start first operation that will fetch and subscribe to delegation record
        // 2. While first operation is in progress, start second operation for same account
        // 3. When first operation tries to unsubscribe, it should detect pending request and skip unsubscription
        // 4. Second operation should complete successfully

        // Use a shared FetchCloner to test deduplication
        // Helper function to spawn a fetch_and_clone task with shared FetchCloner
        let spawn_fetch_task = |fetch_cloner: &Arc<FetchCloner<_, _, _, _>>| {
            let fetch_cloner = fetch_cloner.clone();
            tokio::spawn(async move {
                fetch_cloner
                    .fetch_and_clone_accounts_with_dedup(
                        &[account_pubkey],
                        None,
                        None,
                    )
                    .await
            })
        };

        let fetch_cloner = Arc::new(fetch_cloner);

        // Start multiple concurrent operations on the same account
        let task1 = spawn_fetch_task(&fetch_cloner);
        let task2 = spawn_fetch_task(&fetch_cloner);
        let task3 = spawn_fetch_task(&fetch_cloner);

        // Wait for all operations to complete
        let (result0, result1, result2) =
            tokio::try_join!(task1, task2, task3).unwrap();

        // All operations should succeed (no race condition should cause failures)
        let results = [result0, result1, result2];
        for (i, result) in results.into_iter().enumerate() {
            assert!(result.is_ok(), "Operation {i} failed: {result:?}");
        }

        assert!(accounts_bank.get_account(&account_pubkey).is_some());

        assert_not_subscribed!(
            remote_account_provider,
            &[&account_pubkey, &deleg_record_pubkey]
        );
    }

    #[tokio::test]
    async fn test_fetch_and_clone_with_dedup_concurrent_requests() {
        init_logger();
        let validator_pubkey = random_pubkey();
        let account_owner = random_pubkey();
        const CURRENT_SLOT: u64 = 100;

        let account_pubkey = random_pubkey();
        let account = Account {
            lamports: 2_000_000,
            data: vec![5, 6, 7, 8],
            owner: account_owner,
            executable: false,
            rent_epoch: 0,
        };

        let FetcherTestCtx {
            accounts_bank,
            fetch_cloner,
            ..
        } = setup(
            [(account_pubkey, account.clone())],
            CURRENT_SLOT,
            validator_pubkey,
        )
        .await;

        let fetch_cloner = Arc::new(fetch_cloner);

        // Helper function to spawn fetch task with deduplication
        let spawn_fetch_task = || {
            let fetch_cloner = fetch_cloner.clone();
            tokio::spawn(async move {
                fetch_cloner
                    .fetch_and_clone_accounts_with_dedup(
                        &[account_pubkey],
                        None,
                        None,
                    )
                    .await
            })
        };

        // Spawn multiple concurrent requests for the same account
        let task1 = spawn_fetch_task();
        let task2 = spawn_fetch_task();

        // Both should succeed
        let (result1, result2) = tokio::try_join!(task1, task2).unwrap();
        assert!(result1.is_ok());
        assert!(result2.is_ok());

        // Verify deduplication: should only fetch the account once despite concurrent requests
        assert_eq!(
            fetch_cloner.fetch_count(),
            1,
            "Expected exactly 1 fetch operation for the same account requested concurrently, got {}",
            fetch_cloner.fetch_count()
        );

        // Account should be cloned (only once)
        assert_cloned_undelegated_account!(
            accounts_bank,
            account_pubkey,
            account,
            CURRENT_SLOT,
            account_owner
        );
    }

    #[tokio::test]
    async fn test_undelegation_requested_subscription_behavior() {
        init_logger();
        let validator_pubkey = random_pubkey();
        let account_owner = random_pubkey();
        const CURRENT_SLOT: u64 = 100;

        let account_pubkey = random_pubkey();
        let account = Account {
            lamports: 1_000_000,
            data: vec![1, 2, 3, 4],
            owner: dlp::id(),
            executable: false,
            rent_epoch: 0,
        };

        let FetcherTestCtx {
            remote_account_provider,
            accounts_bank,
            rpc_client,
            fetch_cloner,
            ..
        } = setup(
            [(account_pubkey, account.clone())],
            CURRENT_SLOT,
            validator_pubkey,
        )
        .await;

        add_delegation_record_for(
            &rpc_client,
            account_pubkey,
            validator_pubkey,
            account_owner,
        );

        // Initially fetch and clone the delegated account
        // This should result in no active subscription since it's delegated to us
        let result = fetch_cloner
            .fetch_and_clone_accounts(&[account_pubkey], None, None)
            .await;
        assert!(result.is_ok());

        // Verify account was cloned and is marked as delegated
        assert_cloned_delegated_account!(
            accounts_bank,
            account_pubkey,
            account,
            CURRENT_SLOT,
            account_owner
        );

        // Initially, delegated accounts to us should NOT be subscribed
        assert_not_subscribed!(remote_account_provider, &[&account_pubkey]);

        // Now simulate undelegation request - this should start subscription
        fetch_cloner
            .subscribe_to_account(&account_pubkey)
            .await
            .expect("Failed to subscribe to account for undelegation");

        assert_subscribed!(remote_account_provider, &[&account_pubkey]);
    }

    #[tokio::test]
    async fn test_parallel_fetch_prevention_multiple_accounts() {
        init_logger();
        let validator_pubkey = random_pubkey();
        let account_owner = random_pubkey();
        const CURRENT_SLOT: u64 = 100;

        // Create multiple accounts that will be fetched in parallel
        let account1_pubkey = random_pubkey();
        let account2_pubkey = random_pubkey();
        let account3_pubkey = random_pubkey();

        let account1 = Account {
            lamports: 1_000_000,
            data: vec![1, 2, 3],
            owner: account_owner,
            executable: false,
            rent_epoch: 0,
        };

        let account2 = Account {
            lamports: 2_000_000,
            data: vec![4, 5, 6],
            owner: account_owner,
            executable: false,
            rent_epoch: 0,
        };

        let account3 = Account {
            lamports: 3_000_000,
            data: vec![7, 8, 9],
            owner: account_owner,
            executable: false,
            rent_epoch: 0,
        };

        let accounts = [
            (account1_pubkey, account1.clone()),
            (account2_pubkey, account2.clone()),
            (account3_pubkey, account3.clone()),
        ];

        let FetcherTestCtx {
            accounts_bank,
            fetch_cloner,
            ..
        } = setup(accounts, CURRENT_SLOT, validator_pubkey).await;

        // Use shared FetchCloner to test deduplication across multiple accounts
        // Spawn multiple concurrent requests for overlapping sets of accounts
        let all_accounts =
            vec![account1_pubkey, account2_pubkey, account3_pubkey];
        let accounts_12 = vec![account1_pubkey, account2_pubkey];
        let accounts_23 = vec![account2_pubkey, account3_pubkey];

        let fetch_cloner = Arc::new(fetch_cloner);

        // Helper function to spawn fetch task with deduplication
        let spawn_fetch_task = |accounts: Vec<Pubkey>| {
            let fetch_cloner = fetch_cloner.clone();
            tokio::spawn(async move {
                fetch_cloner
                    .fetch_and_clone_accounts_with_dedup(&accounts, None, None)
                    .await
            })
        };

        let task1 = spawn_fetch_task(all_accounts);
        let task2 = spawn_fetch_task(accounts_12);
        let task3 = spawn_fetch_task(accounts_23);

        // All operations should succeed despite overlapping account requests
        let (result1, result2, result3) =
            tokio::try_join!(task1, task2, task3).unwrap();

        assert!(result1.is_ok(), "Task 1 failed: {result1:?}");
        assert!(result2.is_ok(), "Task 2 failed: {result2:?}");
        assert!(result3.is_ok(), "Task 3 failed: {result3:?}");

        // Verify deduplication: should only fetch 3 unique accounts once each despite overlapping requests
        assert_eq!(fetch_cloner.fetch_count(), 3,);

        // All accounts should be cloned exactly once
        assert_cloned_undelegated_account!(
            accounts_bank,
            account1_pubkey,
            account1,
            CURRENT_SLOT,
            account_owner
        );
        assert_cloned_undelegated_account!(
            accounts_bank,
            account2_pubkey,
            account2,
            CURRENT_SLOT,
            account_owner
        );
        assert_cloned_undelegated_account!(
            accounts_bank,
            account3_pubkey,
            account3,
            CURRENT_SLOT,
            account_owner
        );
    }

    // -----------------
    // Marked Non Existing Accounts
    // -----------------
    #[tokio::test]
    async fn test_fetch_with_some_acounts_marked_as_empty_if_not_found() {
        init_logger();
        let validator_pubkey = random_pubkey();
        let account_owner = random_pubkey();
        const CURRENT_SLOT: u64 = 100;

        // Create one existing account and one non-existing account
        let existing_account_pubkey = random_pubkey();
        let marked_non_existing_account_pubkey = random_pubkey();
        let unmarked_non_existing_account_pubkey = random_pubkey();

        let existing_account = Account {
            lamports: 1_000_000,
            data: vec![1, 2, 3, 4],
            owner: account_owner,
            executable: false,
            rent_epoch: 0,
        };
        let accounts = [(existing_account_pubkey, existing_account.clone())];

        let FetcherTestCtx {
            accounts_bank,
            fetch_cloner,
            remote_account_provider,
            ..
        } = setup(accounts, CURRENT_SLOT, validator_pubkey).await;

        // Configure fetch_cloner to mark some accounts as empty if not found
        fetch_cloner
            .fetch_and_clone_accounts(
                &[
                    existing_account_pubkey,
                    marked_non_existing_account_pubkey,
                    unmarked_non_existing_account_pubkey,
                ],
                Some(&[marked_non_existing_account_pubkey]),
                None,
            )
            .await
            .expect("Fetch and clone failed");

        // Existing account should be cloned normally
        assert_cloned_undelegated_account!(
            accounts_bank,
            existing_account_pubkey,
            existing_account,
            CURRENT_SLOT,
            account_owner
        );

        // Non marked account should not be cloned
        assert_not_cloned!(
            accounts_bank,
            &[unmarked_non_existing_account_pubkey]
        );

        // Marked non-existing account should be cloned as empty
        assert_cloned_undelegated_account!(
            accounts_bank,
            marked_non_existing_account_pubkey,
            Account {
                lamports: 0,
                data: vec![],
                owner: Pubkey::default(),
                executable: false,
                rent_epoch: 0,
            },
            CURRENT_SLOT,
            system_program::id()
        );
        assert_subscribed_without_delegation_record!(
            remote_account_provider,
            &[&marked_non_existing_account_pubkey]
        );
    }
}
