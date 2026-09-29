use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use magicblock_metrics::metrics;
use magicblock_rpc_client::MagicblockRpcClient;
use solana_address_lookup_table_interface::state::AddressLookupTable;
use solana_commitment_config::CommitmentConfig;
use solana_keypair::Keypair;
use solana_message::AddressLookupTableAccount;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use tokio::{
    sync::{Mutex, RwLock},
    time::sleep,
};
use tracing::*;

use crate::{
    TableManiaComputeBudget, TableManiaComputeBudgets,
    error::{TableManiaError, TableManiaResult},
    lookup_table_rc::{LookupTableRc, MAX_ENTRIES_AS_PART_OF_EXTEND},
};

const REMOTE_TABLE_FINALIZATION_DEPTH_SLOTS: u32 = 32;
const REMOTE_TABLE_FINALIZATION_SLOT_TIME: Duration =
    Duration::from_millis(400);
const REMOTE_TABLE_FINALIZATION_BUFFER: Duration = Duration::from_millis(200);
const REMOTE_TABLE_FALLBACK_POLL_INTERVAL: Duration =
    Duration::from_millis(500);
const MAX_ALLOWED_EXTEND_ERRORS: u8 = 5;
/// Keep the create+extend fallback payload minimal after an existing table rejects an extend.
const FALLBACK_NEW_TABLE_INIT_PUBKEYS: usize = 1;
/// Existing-table extend transactions are: compute budget, compute unit price, extend ALT.
const EXTEND_LOOKUP_TABLE_INSTRUCTION_INDEX: u8 = 2;

fn remote_table_finalization_delay() -> Duration {
    REMOTE_TABLE_FINALIZATION_SLOT_TIME
        .saturating_mul(REMOTE_TABLE_FINALIZATION_DEPTH_SLOTS)
        .saturating_add(REMOTE_TABLE_FINALIZATION_BUFFER)
}

// -----------------
// GarbageCollectorConfig
// -----------------

/// Configures the Garbage Collector which deactivates and then closes
/// lookup tables whose pubkeys have been released.
#[derive(Debug, Clone)]
pub struct GarbageCollectorConfig {
    /// The interval at which to check for tables to deactivate.
    pub deactivate_interval_ms: u64,
    /// The interval at which to check for deactivated tables to close.
    pub close_interval_ms: u64,
}

impl Default for GarbageCollectorConfig {
    fn default() -> Self {
        Self {
            deactivate_interval_ms: 1_000,
            close_interval_ms: 5_000,
        }
    }
}

#[derive(Clone)]
pub struct TableMania {
    pub active_tables: Arc<RwLock<Vec<LookupTableRc>>>,
    released_tables: Arc<Mutex<Vec<LookupTableRc>>>,
    authority_pubkey: Pubkey,
    pub rpc_client: MagicblockRpcClient,
    randomize_lookup_table_slot: bool,
    compute_budgets: TableManiaComputeBudgets,
}

#[derive(Debug, Clone, Copy)]
enum ExistingPubkeyAction {
    Reserve,
    LeaveUnreserved,
}

#[derive(Debug, Clone)]
struct MatchingTableReadiness {
    local_keys: HashSet<Pubkey>,
    latest_update_sent_at: Option<Instant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RemoteReadinessTarget {
    wall_clock_deadline: Option<Instant>,
}

#[derive(Debug, PartialEq, Eq)]
enum ExtendTableErrorAction {
    CreateNewTable,
    Retry,
    ReturnError,
}

impl TableMania {
    pub fn new(
        rpc_client: MagicblockRpcClient,
        authority: &Keypair,
        garbage_collector_config: Option<GarbageCollectorConfig>,
    ) -> Self {
        let me = Self {
            active_tables: Arc::<RwLock<Vec<LookupTableRc>>>::default(),
            released_tables: Arc::<Mutex<Vec<LookupTableRc>>>::default(),
            authority_pubkey: authority.pubkey(),
            rpc_client,
            randomize_lookup_table_slot: randomize_lookup_table_slot(),
            compute_budgets: TableManiaComputeBudgets::default(),
        };
        if let Some(config) = garbage_collector_config {
            Self::launch_garbage_collector(
                &me.rpc_client,
                authority,
                me.released_tables.clone(),
                config,
                me.compute_budgets.deactivate.clone(),
                me.compute_budgets.close.clone(),
            );
        }
        me
    }

    /// Returns the number of currently active tables
    pub async fn active_tables_count(&self) -> usize {
        self.active_tables.read().await.len()
    }

    /// Returns the number of released tables
    pub async fn released_tables_count(&self) -> usize {
        self.released_tables.lock().await.len()
    }

    /// Returns the addresses of all tables currently active
    pub async fn active_table_addresses(&self) -> Vec<Pubkey> {
        let mut addresses = Vec::new();

        for table in self.active_tables.read().await.iter() {
            addresses.push(*table.table_address());
        }

        addresses
    }

    /// Returns the addresses of all released tables
    pub async fn released_table_addresses(&self) -> Vec<Pubkey> {
        self.released_tables
            .lock()
            .await
            .iter()
            .map(|table| *table.table_address())
            .collect()
    }

    /// Returns the addresses stored accross all active tables
    pub async fn active_table_pubkeys(&self) -> Vec<Pubkey> {
        let mut pubkeys = Vec::new();
        for table in self.active_tables.read().await.iter() {
            if let Some(pks) = table.pubkeys() {
                pubkeys.extend(pks.keys());
            }
        }
        pubkeys
    }

    /// Returns the refcount of a pubkey if it exists in any active table
    /// - *pubkey* to query refcount for
    /// - *returns* `Some(refcount)` if the pubkey exists in any table, `None` otherwise
    pub async fn get_pubkey_refcount(&self, pubkey: &Pubkey) -> Option<usize> {
        for table in self.active_tables.read().await.iter() {
            if let Some(refcount) = table.get_refcount(pubkey) {
                return Some(refcount);
            }
        }
        None
    }
    // -----------------
    // Reserve
    // -----------------
    pub async fn reserve_pubkeys(
        &self,
        authority: &Keypair,
        pubkeys: &HashSet<Pubkey>,
    ) -> TableManiaResult<()> {
        let mut remaining = HashSet::new();
        // 1. Add reservations for pubkeys that are already in one of the tables
        for pubkey in pubkeys {
            if !self.reserve_pubkey(pubkey).await {
                remaining.insert(*pubkey);
            }
        }

        // 2. Add new reservations for pubkeys that are not in any table
        self.reserve_new_pubkeys(
            authority,
            &remaining,
            ExistingPubkeyAction::Reserve,
        )
        .await
    }

    /// Ensures that pubkeys exist in any active table without increasing reference counts.
    /// If tables for any pubkeys do not exist, creates them using the same transaction
    /// logic as when reserving pubkeys.
    ///
    /// This method awaits the transaction outcome and returns once all pubkeys are part of tables.
    ///
    /// - *authority* - The authority keypair to use for creating tables if needed
    /// - *pubkeys* - The pubkeys to ensure exist in tables
    /// - *returns* `Ok(())` if all pubkeys are now part of tables
    pub async fn ensure_pubkeys_table(
        &self,
        authority: &Keypair,
        pubkeys: &HashSet<Pubkey>,
    ) -> TableManiaResult<()> {
        let mut remaining = HashSet::new();

        // 1. Check which pubkeys already exist in any table
        {
            let active_tables = self.active_tables.read().await;
            for pubkey in pubkeys {
                let mut found = false;
                for table in active_tables.iter() {
                    if table.contains_key(pubkey) {
                        found = true;
                        break;
                    }
                }
                if !found {
                    remaining.insert(*pubkey);
                }
            }
        } // Drop the lock here before calling reserve_new_pubkeys

        // 2. If any pubkeys dont exist, create tables for them
        if !remaining.is_empty() {
            self.reserve_new_pubkeys(
                authority,
                &remaining,
                ExistingPubkeyAction::LeaveUnreserved,
            )
            .await?;
        }

        Ok(())
    }
    /// Tries to find a table that holds this pubkey already and reserves it.
    /// - *pubkey* to reserve
    /// - *returns* `true` if the pubkey could be reserved
    #[instrument(skip(self), fields(pubkey = %pubkey))]
    async fn reserve_pubkey(&self, pubkey: &Pubkey) -> bool {
        for table in self.active_tables.read().await.iter() {
            if table.reserve_pubkey(pubkey) {
                trace!("Added reservation to table");
                return true;
            }
        }
        trace!("No table found for reservation");
        false
    }

    /// Reserves pubkeys that haven't been found in any of the active tables.
    /// Thus this is considered the first reservation for these pubkeys and thus includes
    /// initializing/extending actual lookup tables on chain.
    #[instrument(
        skip(self, authority, pubkeys),
        fields(pubkey_count = pubkeys.len())
    )]
    async fn reserve_new_pubkeys(
        &self,
        authority: &Keypair,
        pubkeys: &HashSet<Pubkey>,
        existing_pubkey_action: ExistingPubkeyAction,
    ) -> TableManiaResult<()> {
        self.check_authority(authority)?;

        let mut remaining = pubkeys.iter().cloned().collect::<Vec<_>>();
        let mut tables_used = HashSet::new();

        let mut extend_errors = 0;

        // Keep trying to store pubkeys until we're done
        while !remaining.is_empty() {
            // First try to use existing tables
            let mut stored_in_existing = false;
            let mut force_new_table_after = None;
            {
                // Taking a write lock here to prevent multiple tasks from
                // updating tables at the same time
                let active_tables_write_lock = self.active_tables.write().await;

                // Try to use the last table if it's not full
                if let Some(table) = active_tables_write_lock.last()
                    && !table.is_full()
                {
                    if let Err(err) = self
                        .extend_table(
                            table,
                            authority,
                            &mut remaining,
                            &mut tables_used,
                            existing_pubkey_action,
                        )
                        .await
                    {
                        match Self::handle_extend_table_error(
                            &err,
                            &mut extend_errors,
                        ) {
                            ExtendTableErrorAction::CreateNewTable => {
                                error!(
                                    error = ?err,
                                    table_address = %table.table_address(),
                                    "Failed to extend table with invalid instruction data; creating a new table"
                                );
                                table.mark_non_extendable();
                                force_new_table_after =
                                    Some(*table.table_address());
                            }
                            ExtendTableErrorAction::Retry => {
                                error!(
                                    error = ?err,
                                    table_address = %table.table_address(),
                                    "Failed to extend table"
                                );
                            }
                            ExtendTableErrorAction::ReturnError => {
                                return Err(err);
                            }
                        }
                    } else {
                        stored_in_existing = true;
                    }
                }
            }

            // If we couldn't use existing tables, we need to create a new one
            if !stored_in_existing && !remaining.is_empty() {
                // We write lock the active tables to ensure that while we create a new
                // table the requests looking for an existing table to extend are blocked
                let mut active_tables_write_lock =
                    self.active_tables.write().await;

                // Double-check if a new table was created while we were waiting for the lock
                if let Some(table) = active_tables_write_lock.last()
                    && !table.is_full()
                    && force_new_table_after != Some(*table.table_address())
                {
                    // Another task created a table we can use, so drop the write lock
                    // and try again with the read lock
                    drop(active_tables_write_lock);
                    continue;
                }

                // Create a new table and add it to active_tables
                let initial_pubkey_limit = if force_new_table_after.is_some() {
                    FALLBACK_NEW_TABLE_INIT_PUBKEYS
                } else {
                    MAX_ENTRIES_AS_PART_OF_EXTEND as usize
                };
                let table = self
                    .create_new_table_and_extend(
                        authority,
                        &mut remaining,
                        initial_pubkey_limit,
                    )
                    .await?;

                tables_used.insert(*table.table_address());
                active_tables_write_lock.push(table);
            }

            // If we've stored all pubkeys, we're done
            if remaining.is_empty() {
                break;
            }
        }

        Ok(())
    }

    fn handle_extend_table_error(
        err: &TableManiaError,
        extend_errors: &mut u8,
    ) -> ExtendTableErrorAction {
        if err.is_sent_transaction_invalid_instruction_data_at(
            EXTEND_LOOKUP_TABLE_INSTRUCTION_INDEX,
        ) {
            return ExtendTableErrorAction::CreateNewTable;
        }
        if *extend_errors < MAX_ALLOWED_EXTEND_ERRORS {
            *extend_errors += 1;
            return ExtendTableErrorAction::Retry;
        }
        ExtendTableErrorAction::ReturnError
    }

    fn filter_pubkeys_present_in_table(
        table: &LookupTableRc,
        remaining: &mut Vec<Pubkey>,
        existing_pubkey_action: ExistingPubkeyAction,
    ) {
        remaining.retain(|pk| match existing_pubkey_action {
            ExistingPubkeyAction::Reserve => !table.reserve_pubkey(pk),
            ExistingPubkeyAction::LeaveUnreserved => !table.contains_key(pk),
        });
    }

    /// Extends the table to store as many of the provided pubkeys as possile.
    /// The stored pubkeys are removed from the `remaining` vector.
    /// If successful the table addres is added to the `tables_used` set.
    /// Returns `true` if the table is full after adding the pubkeys
    #[instrument(
        skip(self, table, authority, remaining, tables_used),
        fields(
            table_address = %table.table_address(),
            remaining_count = remaining.len(),
            stored_count = tracing::field::Empty
        )
    )]
    async fn extend_table(
        &self,
        table: &LookupTableRc,
        authority: &Keypair,
        remaining: &mut Vec<Pubkey>,
        tables_used: &mut HashSet<Pubkey>,
        existing_pubkey_action: ExistingPubkeyAction,
    ) -> TableManiaResult<()> {
        let remaining_len = remaining.len();
        let storing_len =
            remaining_len.min(MAX_ENTRIES_AS_PART_OF_EXTEND as usize);
        trace!("Extending existing table");
        if table.is_deactivated() {
            return Err(TableManiaError::CannotExtendDeactivatedTable(
                *table.table_address(),
            ));
        };

        let storing = remaining[..storing_len].to_vec();
        let stored = match table
            .extend_respecting_capacity(
                &self.rpc_client,
                authority,
                &storing,
                &self.compute_budgets.extend,
            )
            .await
        {
            Ok(stored) => stored,
            Err(err) => {
                // Extend failed; chain may be ahead of local (a prior extend
                // landed but its outcome was lost). Reconcile so the next
                // outer iteration sees accurate fullness, and reserve any of
                // `storing` that turned out to already be on chain.
                if table.reconcile_with_chain(&self.rpc_client).await.is_ok() {
                    Self::filter_pubkeys_present_in_table(
                        table,
                        remaining,
                        existing_pubkey_action,
                    );
                }
                return Err(err);
            }
        };
        let stored_len = stored.len();
        tracing::Span::current().record("stored_count", stored_len);
        trace!("Pubkeys stored");
        tables_used.insert(*table.table_address());
        remaining.retain(|pk| !stored.contains(pk));

        let remaining_count = remaining.len();
        tracing::Span::current().record("remaining_count", remaining_count);
        trace!("Progress update in reservation");

        #[cfg(debug_assertions)]
        {
            for pk in &stored {
                if !table.contains_key(pk) {
                    panic!(
                        "Pubkey {pk} stored as part of {} was not extended in table {} with {} items.",
                        stored.len(),
                        table.table_address(),
                        table.pubkeys().map(|x| x.len()).unwrap_or(0)
                    );
                }
            }
        }

        Ok(())
    }

    async fn create_new_table_and_extend(
        &self,
        authority: &Keypair,
        pubkeys: &mut Vec<Pubkey>,
        initial_pubkey_limit: usize,
    ) -> TableManiaResult<LookupTableRc> {
        static SUB_SLOT: AtomicU64 = AtomicU64::new(0);

        let pubkeys_len = pubkeys.len();
        let slot = self.rpc_client.get_slot().await?;

        if self.randomize_lookup_table_slot {
            use rand::Rng;
            let mut rng = rand::rng();
            let random_slot = rng.random_range(0..=u64::MAX);
            SUB_SLOT.store(random_slot, Ordering::Relaxed);
        } else {
            static LAST_SLOT: AtomicU64 = AtomicU64::new(0);
            let prev_last_slot = LAST_SLOT.swap(slot, Ordering::Relaxed);
            if prev_last_slot != slot {
                SUB_SLOT.store(0, Ordering::Relaxed);
            } else {
                SUB_SLOT.fetch_add(1, Ordering::Relaxed);
            }
        }

        let len = pubkeys_len
            .min(initial_pubkey_limit)
            .min(MAX_ENTRIES_AS_PART_OF_EXTEND as usize);
        let table = LookupTableRc::init(
            &self.rpc_client,
            authority,
            slot,
            SUB_SLOT.load(Ordering::Relaxed),
            &pubkeys[..len],
            &self.compute_budgets.init,
        )
        .await?;
        pubkeys.retain_mut(|pk| !table.contains_key(pk));

        trace!("Created new table and stored pubkeys");
        Ok(table)
    }

    // -----------------
    // Release
    // -----------------
    pub async fn release_pubkeys(&self, pubkeys: &HashSet<Pubkey>) {
        for pubkey in pubkeys {
            self.release_pubkey(pubkey).await;
        }
        // While we hold the write lock on the active tables no one can make
        // a reservation on any of them until we mark them for deactivation.
        let mut active_tables = self.active_tables.write().await;
        let mut still_active = Vec::new();
        for table in active_tables.drain(..) {
            if table.has_reservations() {
                still_active.push(table);
            } else {
                self.released_tables.lock().await.push(table);
            }
        }
        for table in still_active.into_iter() {
            active_tables.push(table);
        }
    }

    #[instrument(skip(self), fields(pubkey = %pubkey))]
    async fn release_pubkey(&self, pubkey: &Pubkey) {
        for table in self.active_tables.read().await.iter() {
            if table.release_pubkey(pubkey) {
                trace!("Released reservation from table");
                return;
            }
        }
        trace!("No table found for release");
    }

    // -----------------
    // Tables for Reserved Pubkeys
    // -----------------

    fn remote_readiness_target<'a>(
        matching_tables: impl Iterator<Item = &'a MatchingTableReadiness>,
    ) -> RemoteReadinessTarget {
        let delay = remote_table_finalization_delay();
        let mut wall_clock_deadline = None;

        for table in matching_tables {
            if let Some(update_sent_at) = table.latest_update_sent_at {
                let target = update_sent_at + delay;
                wall_clock_deadline = Some(
                    wall_clock_deadline
                        .map_or(target, |x: Instant| x.max(target)),
                );
            }
        }

        RemoteReadinessTarget {
            wall_clock_deadline,
        }
    }

    async fn wait_until_remote_readiness_target(
        target: RemoteReadinessTarget,
        wait_for_remote_table_match: Duration,
    ) {
        let Some(deadline) = target.wall_clock_deadline else {
            return;
        };

        let now = Instant::now();
        if deadline <= now {
            return;
        }

        let remaining_until_deadline = deadline.duration_since(now);
        let delay = remaining_until_deadline.min(wait_for_remote_table_match);
        sleep(delay).await;
    }

    /// Attempts to find a table that holds each of the pubkeys.
    /// It only returns once the needed pubkeys are also present remotely in the
    /// finalized table accounts.
    ///
    /// - *pubkeys* to find tables for
    /// - *wait_for_local_table_match* how long to wait for local tables to match which
    ///   means the [Self::reserve_pubkeys] was completed including any transactions that were sent
    /// - *wait_for_remote_table_match* how long to wait for remote tables to include the
    ///   matched pubkeys
    #[instrument(
        skip(self),
        fields(
            pubkey_count = pubkeys.len(),
            table_count = tracing::field::Empty,
            timeout_ms = tracing::field::Empty,
            current_slot = tracing::field::Empty,
        )
    )]
    pub async fn try_get_active_address_lookup_table_accounts(
        &self,
        pubkeys: &HashSet<Pubkey>,
        wait_for_local_table_match: Duration,
        wait_for_remote_table_match: Duration,
    ) -> TableManiaResult<Vec<AddressLookupTableAccount>> {
        // 1. Wait until all keys are present in a local table
        let matching_tables = {
            let start = Instant::now();
            tracing::Span::current().record(
                "timeout_ms",
                wait_for_local_table_match.as_millis() as u64,
            );
            loop {
                {
                    let active_local_tables = self.active_tables.read().await;
                    let mut keys_to_match = pubkeys.clone();
                    let mut matching_tables = HashMap::new();
                    for table in active_local_tables.iter() {
                        let matching_keys = table.match_pubkeys(&keys_to_match);
                        if !matching_keys.is_empty() {
                            keys_to_match
                                .retain(|pk| !matching_keys.contains(pk));
                            matching_tables.insert(
                                *table.table_address(),
                                MatchingTableReadiness {
                                    latest_update_sent_at: table
                                        .latest_update_sent_at_for(
                                            &matching_keys,
                                        ),
                                    local_keys: matching_keys,
                                },
                            );
                        }
                    }
                    if keys_to_match.is_empty() {
                        break matching_tables;
                    }
                    trace!("Waiting for local tables to match");
                }
                if start.elapsed() > wait_for_local_table_match {
                    error!("Timed out waiting for local tables to match");
                    return Err(
                        TableManiaError::TimedOutWaitingForRemoteTablesToUpdate(
                            format!("{:?}", pubkeys),
                        ),
                    );
                }

                sleep(Duration::from_millis(200)).await;
            }
        };
        tracing::Span::current().record("table_count", matching_tables.len());
        tracing::Span::current().record(
            "timeout_ms",
            wait_for_remote_table_match.as_millis() as u64,
        );

        // 2. Ensure that all matching keys are also present remotely and have been finalized
        let remote_tables = {
            let matching_table_keys =
                matching_tables.keys().cloned().collect::<Vec<_>>();

            let start = Instant::now();
            let mut last_wait_log = Instant::now();
            let table_keys_str = matching_table_keys
                .iter()
                .map(|x| x.to_string())
                .collect::<Vec<_>>()
                .join(", ");

            let readiness_target =
                Self::remote_readiness_target(matching_tables.values());
            let wait_delay_ms = readiness_target
                .wall_clock_deadline
                .map(|deadline| {
                    deadline
                        .saturating_duration_since(Instant::now())
                        .as_millis() as u64
                })
                .unwrap_or(0);
            debug!(
                wait_delay_ms,
                "Delaying first finalized remote table fetch using required-pubkey transaction send-time estimate"
            );
            Self::wait_until_remote_readiness_target(
                readiness_target,
                wait_for_remote_table_match,
            )
            .await;

            loop {
                metrics::inc_table_mania_a_count();
                // Fetch the tables from chain
                let remote_table_accs = self
                    .rpc_client
                    .get_multiple_accounts_with_commitment(
                        &matching_table_keys,
                        // For lookup tables to be useful in a transaction all create/extend
                        // transactions on the table need to be finalized
                        CommitmentConfig::finalized(),
                        None,
                    )
                    .await?;

                let remote_tables = remote_table_accs
                    .into_iter()
                    .enumerate()
                    .flat_map(|(idx, acc)| {
                        acc.and_then(
                            |acc| match AddressLookupTable::deserialize(
                                &acc.data,
                            ) {
                                Ok(table) => Some((
                                    matching_table_keys[idx],
                                    table.addresses.to_vec(),
                                )),
                                Err(err) => {
                                    error!(error = ?err, "Failed to deserialize table");
                                    None
                                }
                            },
                        )
                    })
                    .collect::<HashMap<_, _>>();

                // Ensure we got the same amount of tables
                if remote_tables.len() == matching_tables.len() {
                    // And that all locally matched keys are in the finalized remote table
                    let all_matches_are_remote =
                        matching_tables.iter().all(|(address, readiness)| {
                            remote_tables.get(address).is_some_and(
                                |remote_keys| {
                                    readiness
                                        .local_keys
                                        .iter()
                                        .all(|pk| remote_keys.contains(pk))
                                },
                            )
                        });
                    if all_matches_are_remote {
                        break remote_tables;
                    }
                }

                if start.elapsed() > wait_for_remote_table_match {
                    error!(
                        timeout_ms =
                            wait_for_remote_table_match.as_millis() as u64,
                        elapsed_ms = start.elapsed().as_millis() as u64,
                        "Timed out waiting for remote tables to match"
                    );
                    return Err(
                        TableManiaError::TimedOutWaitingForRemoteTablesToUpdate(
                            table_keys_str,
                        ),
                    );
                }

                sleep(REMOTE_TABLE_FALLBACK_POLL_INTERVAL).await;
                if last_wait_log.elapsed() > Duration::from_secs(8) {
                    debug!("Still waiting for remote tables");
                    last_wait_log = Instant::now();
                }
            }
        };

        Ok(matching_tables
            .into_keys()
            .map(|address| AddressLookupTableAccount {
                key: address,
                // SAFETY: we confirmed above that we have a remote table for all matching
                // tables and that they contain the addresses we need
                addresses: remote_tables.get(&address).unwrap().to_vec(),
            })
            .collect())
    }

    // -----------------
    // Garbage Collector
    // -----------------

    // For deactivate/close operations running as part of the garbage collector task
    // we only log errors since there is no reasonable way to handle them.
    // The next cycle will try the operation again so in case chain was congested
    // the problem should resolve itself.
    // Otherwise we can run a tool later to manually deactivate + close tables.
    fn launch_garbage_collector(
        rpc_client: &MagicblockRpcClient,
        authority: &Keypair,
        released_tables: Arc<Mutex<Vec<LookupTableRc>>>,
        config: GarbageCollectorConfig,
        deactivate_compute_budget: TableManiaComputeBudget,
        close_compute_budget: TableManiaComputeBudget,
    ) -> tokio::task::JoinHandle<()> {
        let rpc_client = rpc_client.clone();
        let authority = authority.insecure_clone();

        tokio::spawn(async move {
            let mut last_deactivate = tokio::time::Instant::now();
            let mut last_close = tokio::time::Instant::now();
            let mut sleep_ms =
                config.deactivate_interval_ms.min(config.close_interval_ms);
            loop {
                let now = tokio::time::Instant::now();
                if now
                    .duration_since(last_deactivate)
                    .as_millis()
                    .try_into()
                    .unwrap_or(u64::MAX)
                    >= config.deactivate_interval_ms
                {
                    Self::deactivate_tables(
                        &rpc_client,
                        &authority,
                        &released_tables,
                        &deactivate_compute_budget,
                    )
                    .await;
                    last_deactivate = now;
                    sleep_ms = sleep_ms.min(config.deactivate_interval_ms);
                }
                if now
                    .duration_since(last_close)
                    .as_millis()
                    .try_into()
                    .unwrap_or(u64::MAX)
                    >= config.close_interval_ms
                {
                    Self::close_tables(
                        &rpc_client,
                        &authority,
                        &released_tables,
                        &close_compute_budget,
                    )
                    .await;
                    last_close = now;
                    sleep_ms = sleep_ms.min(config.close_interval_ms);
                }

                tokio::time::sleep(tokio::time::Duration::from_millis(
                    sleep_ms,
                ))
                .await;
            }
        })
    }

    /// Deactivates tables that were previously released
    #[instrument(skip(rpc_client, authority, released_tables, compute_budget), fields(table_count = tracing::field::Empty, table_address = tracing::field::Empty))]
    async fn deactivate_tables(
        rpc_client: &MagicblockRpcClient,
        authority: &Keypair,
        released_tables: &Mutex<Vec<LookupTableRc>>,
        compute_budget: &TableManiaComputeBudget,
    ) {
        let table_count = released_tables
            .lock()
            .await
            .iter()
            .filter(|x| !x.deactivate_triggered())
            .count();
        tracing::Span::current().record("table_count", table_count);
        for table in released_tables
            .lock()
            .await
            .iter_mut()
            .filter(|x| !x.deactivate_triggered())
        {
            tracing::Span::current()
                .record("table_address", table.table_address().to_string());
            // We don't bubble errors as there is no reasonable way to handle them.
            // Instead the next GC cycle will try again to deactivate the table.
            let _ = table
                .deactivate(rpc_client, authority, compute_budget)
                .await
                .inspect_err(|err| {
                    error!(
                        error = ?err,
                        "Failed to deactivate table"
                    )
                });
        }
    }

    /// Closes tables that were previously released and deactivated.
    #[instrument(
        skip(rpc_client, authority, released_tables, compute_budget),
        fields(
            deactivated_table_count = tracing::field::Empty,
            current_slot = tracing::field::Empty,
        )
    )]
    async fn close_tables(
        rpc_client: &MagicblockRpcClient,
        authority: &Keypair,
        released_tables: &Mutex<Vec<LookupTableRc>>,
        compute_budget: &TableManiaComputeBudget,
    ) {
        // Avoid doing any work if there aren't any deactivated tables to close.
        // Mainly we avoid the `get_slot` call in that case
        let has_deactivated_tables = released_tables
            .lock()
            .await
            .iter()
            .any(|x| x.deactivate_triggered());
        if !has_deactivated_tables {
            return;
        }

        let deactivated_count = released_tables
            .lock()
            .await
            .iter()
            .filter(|x| x.deactivate_triggered())
            .count();
        tracing::Span::current()
            .record("deactivated_table_count", deactivated_count);

        let Ok(latest_slot) = rpc_client.get_slot().await.inspect_err(
            |err| error!(error = ?err, "Failed to get latest slot"),
        ) else {
            return;
        };

        tracing::Span::current().record("current_slot", latest_slot);

        let mut closed_tables = vec![];
        {
            for deactivated_table in released_tables
                .lock()
                .await
                .iter_mut()
                .filter(|x| x.deactivate_triggered())
            {
                // NOTE: [LookupTable::close] will only close the table if it was deactivated
                //        according to the provided slot
                // We don't bubble errors as there is no reasonable way to handle them.
                // Instead the next GC cycle will try again to close the table.
                match deactivated_table
                    .close(
                        rpc_client,
                        authority,
                        Some(latest_slot),
                        compute_budget,
                    )
                    .await
                {
                    Ok((closed, _)) if closed => {
                        closed_tables.push(*deactivated_table.table_address())
                    }
                    Ok(_) => {
                        // Table not ready to be closed
                    }
                    Err(err) => error!(
                        error = ?err,
                        table_address = %deactivated_table.table_address(),
                        "Failed to close table"
                    ),
                };
            }
        }
        released_tables
            .lock()
            .await
            .retain(|x| !closed_tables.contains(x.table_address()));
    }

    // -----------------
    // Checks
    // -----------------
    fn check_authority(&self, authority: &Keypair) -> TableManiaResult<()> {
        if authority.pubkey() != self.authority_pubkey {
            return Err(TableManiaError::InvalidAuthority(
                authority.pubkey(),
                self.authority_pubkey,
            ));
        }
        Ok(())
    }
}

fn randomize_lookup_table_slot() -> bool {
    #[cfg(feature = "randomize_lookup_table_slot")]
    {
        true
    }
    #[cfg(not(feature = "randomize_lookup_table_slot"))]
    {
        std::env::var("RANDOMIZE_LOOKUP_TABLE_SLOT").is_ok()
    }
}

#[cfg(test)]
mod tests {
    use magicblock_rpc_client::MagicBlockRpcClientError;
    use solana_instruction::error::InstructionError;
    use solana_signature::Signature;
    use solana_transaction_error::TransactionError;

    use super::{
        ExtendTableErrorAction, MAX_ALLOWED_EXTEND_ERRORS, TableMania,
        TableManiaError,
    };

    fn sent_transaction_error(
        instruction_error: InstructionError,
    ) -> TableManiaError {
        TableManiaError::MagicBlockRpcClientError(
            MagicBlockRpcClientError::SentTransactionError(
                TransactionError::InstructionError(2, instruction_error),
                Signature::default(),
            ),
        )
    }

    #[test]
    fn invalid_instruction_data_forces_new_table_without_retrying() {
        let mut extend_errors = 0;
        let err =
            sent_transaction_error(InstructionError::InvalidInstructionData);

        assert_eq!(
            TableMania::handle_extend_table_error(&err, &mut extend_errors),
            ExtendTableErrorAction::CreateNewTable
        );
        assert_eq!(extend_errors, 0);
    }

    #[test]
    fn other_extend_errors_use_retry_budget() {
        let mut extend_errors = 0;
        let err = sent_transaction_error(InstructionError::InvalidArgument);

        for expected_errors in 1..=MAX_ALLOWED_EXTEND_ERRORS {
            assert_eq!(
                TableMania::handle_extend_table_error(&err, &mut extend_errors),
                ExtendTableErrorAction::Retry
            );
            assert_eq!(extend_errors, expected_errors);
        }

        assert_eq!(
            TableMania::handle_extend_table_error(&err, &mut extend_errors),
            ExtendTableErrorAction::ReturnError
        );
    }
}
