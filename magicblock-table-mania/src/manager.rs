use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use log::*;
use magicblock_rpc_client::MagicblockRpcClient;
use solana_pubkey::Pubkey;
use solana_sdk::{
    address_lookup_table::state::AddressLookupTable,
    commitment_config::CommitmentConfig, message::AddressLookupTableAccount,
    signature::Keypair, signer::Signer,
};
use tokio::{
    sync::{Mutex, RwLock},
    time::sleep,
};

use crate::{
    error::{TableManiaError, TableManiaResult},
    lookup_table_rc::{LookupTableRc, MAX_ENTRIES_AS_PART_OF_EXTEND},
    TableManiaComputeBudgets,
};

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
        self.reserve_new_pubkeys(authority, &remaining).await
    }

    /// Tries to find a table that holds this pubkey already and reserves it.
    /// - *pubkey* to reserve
    /// - *returns* `true` if the pubkey could be reserved
    async fn reserve_pubkey(&self, pubkey: &Pubkey) -> bool {
        for table in self.active_tables.read().await.iter() {
            if table.reserve_pubkey(pubkey) {
                trace!(
                    "Added reservation for pubkey {} to table {}",
                    pubkey,
                    table.table_address()
                );
                return true;
            }
        }
        trace!("No table found for which we can reserve pubkey {}", pubkey);
        false
    }

    /// Reserves pubkeys that haven't been found in any of the active tables.
    /// Thus this is considered the first reservation for these pubkeys and thus includes
    /// initializing/extending actual lookup tables on chain.
    async fn reserve_new_pubkeys(
        &self,
        authority: &Keypair,
        pubkeys: &HashSet<Pubkey>,
    ) -> TableManiaResult<()> {
        self.check_authority(authority)?;

        let mut remaining = pubkeys.iter().cloned().collect::<Vec<_>>();
        let mut tables_used = HashSet::new();

        const MAX_ALLOWED_EXTEND_ERRORS: u8 = 5;
        let mut extend_errors = 0;

        // Keep trying to store pubkeys until we're done
        while !remaining.is_empty() {
            // First try to use existing tables
            let mut stored_in_existing = false;
            {
                // Taking a write lock here to prevent multiple tasks from
                // updating tables at the same time
                let active_tables_write_lock = self.active_tables.write().await;

                // Try to use the last table if it's not full
                if let Some(table) = active_tables_write_lock.last() {
                    if !table.is_full() {
                        if let Err(err) = self
                            .extend_table(
                                table,
                                authority,
                                &mut remaining,
                                &mut tables_used,
                            )
                            .await
                        {
                            error!(
                                "Error extending table {}: {:?}",
                                table.table_address(),
                                err
                            );
                            if extend_errors >= MAX_ALLOWED_EXTEND_ERRORS {
                                extend_errors += 1;
                            } else {
                                return Err(err);
                            }
                        } else {
                            stored_in_existing = true;
                        }
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
                if let Some(table) = active_tables_write_lock.last() {
                    if !table.is_full() {
                        // Another task created a table we can use, so drop the write lock
                        // and try again with the read lock
                        drop(active_tables_write_lock);
                        continue;
                    }
                }

                // Create a new table and add it to active_tables
                let table = self
                    .create_new_table_and_extend(authority, &mut remaining)
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

    /// Extends the table to store as many of the provided pubkeys as possile.
    /// The stored pubkeys are removed from the `remaining` vector.
    /// If successful the table addres is added to the `tables_used` set.
    /// Returns `true` if the table is full after adding the pubkeys
    async fn extend_table(
        &self,
        table: &LookupTableRc,
        authority: &Keypair,
        remaining: &mut Vec<Pubkey>,
        tables_used: &mut HashSet<Pubkey>,
    ) -> TableManiaResult<()> {
        let remaining_len = remaining.len();
        let storing_len =
            remaining_len.min(MAX_ENTRIES_AS_PART_OF_EXTEND as usize);
        trace!(
            "Adding {}/{} pubkeys to existing table {}",
            storing_len,
            remaining_len,
            table.table_address()
        );
        let Some(table_addresses_count) = table.pubkeys().map(|x| x.len())
        else {
            return Err(TableManiaError::CannotExtendDeactivatedTable(
                *table.table_address(),
            ));
        };

        let storing = remaining[..storing_len].to_vec();
        let stored = table
            .extend_respecting_capacity(
                &self.rpc_client,
                authority,
                &storing,
                &self.compute_budgets.extend,
            )
            .await?;
        trace!("Stored {}", stored.len());
        tables_used.insert(*table.table_address());
        remaining.retain(|pk| !stored.contains(pk));

        let stored_count = remaining_len - remaining.len();
        trace!("Stored {}, remaining: {}", stored_count, remaining.len());

        debug_assert_eq!(
            table_addresses_count + stored_count,
            table.pubkeys().unwrap().len()
        );

        Ok(())
    }

    async fn create_new_table_and_extend(
        &self,
        authority: &Keypair,
        pubkeys: &mut Vec<Pubkey>,
    ) -> TableManiaResult<LookupTableRc> {
        static SUB_SLOT: AtomicU64 = AtomicU64::new(0);

        let pubkeys_len = pubkeys.len();
        let slot = self.rpc_client.get_slot().await?;

        if self.randomize_lookup_table_slot {
            use rand::Rng;
            let mut rng = rand::thread_rng();
            let random_slot = rng.gen_range(0..=u64::MAX);
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

        let len = pubkeys_len.min(MAX_ENTRIES_AS_PART_OF_EXTEND as usize);
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

        trace!(
            "Created new table and stored {}/{} pubkeys. {}",
            len,
            pubkeys_len,
            table.table_address()
        );
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

    async fn release_pubkey(&self, pubkey: &Pubkey) {
        for table in self.active_tables.read().await.iter() {
            if table.release_pubkey(pubkey) {
                trace!(
                    "Removed reservation for pubkey {} from table {}",
                    pubkey,
                    table.table_address()
                );
                return;
            }
        }
        trace!("No table found for which we can release pubkey {}", pubkey);
    }

    // -----------------
    // Tables for Reserved Pubkeys
    // -----------------

    /// Attempts to find a table that holds each of the pubkeys.
    /// It only returns once the needed pubkeys are also present remotely in the
    /// finalized table accounts.
    ///
    /// - *pubkeys* to find tables for
    /// - *wait_for_local_table_match* how long to wait for local tables to match which
    ///   means the [Self::reserve_pubkeys] was completed including any transactions that were sent
    /// - *wait_for_remote_table_match* how long to wait for remote tables to include the
    ///   matched pubkeys
    pub async fn try_get_active_address_lookup_table_accounts(
        &self,
        pubkeys: &HashSet<Pubkey>,
        wait_for_local_table_match: Duration,
        wait_for_remote_table_match: Duration,
    ) -> TableManiaResult<Vec<AddressLookupTableAccount>> {
        // 1. Wait until all keys are present in a local table
        let matching_tables = {
            let start = Instant::now();
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
                            matching_tables
                                .insert(*table.table_address(), matching_keys);
                        }
                    }
                    if keys_to_match.is_empty() {
                        break matching_tables;
                    }
                    trace!(
                        "Matched {}/{} pubkeys",
                        pubkeys.len() - keys_to_match.len(),
                        pubkeys.len()
                    );
                }
                if start.elapsed() > wait_for_local_table_match {
                    error!(
                        "Timed out waiting for local tables to match requested keys: {:?} for {:?}",
                        pubkeys,
                        wait_for_local_table_match,

                    );
                    return Err(
                        TableManiaError::TimedOutWaitingForRemoteTablesToUpdate(
                            format!("{:?}", pubkeys),
                        ),
                    );
                }

                sleep(Duration::from_millis(200)).await;
            }
        };

        // 2. Ensure that all matching keys are also present remotely and have been finalized
        let remote_tables = {
            let mut last_slot = self.rpc_client.get_slot().await?;

            let matching_table_keys =
                matching_tables.keys().cloned().collect::<Vec<_>>();

            let start = Instant::now();
            let table_keys_str = matching_table_keys
                .iter()
                .map(|x| x.to_string())
                .collect::<Vec<_>>()
                .join(", ");

            loop {
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
                                    error!(
                                        "Failed to deserialize table {}: {:?}",
                                        matching_table_keys[idx], err
                                    );
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
                        matching_tables.iter().all(|(address, local_keys)| {
                            remote_tables.get(address).is_some_and(
                                |remote_keys| {
                                    local_keys
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
                        "Timed out waiting for remote tables to match local tables for {:?}. \
                        Local: {:#?}\nRemote: {:#?}",
                        wait_for_remote_table_match, matching_tables, remote_tables
                    );
                    return Err(
                        TableManiaError::TimedOutWaitingForRemoteTablesToUpdate(
                            table_keys_str,
                        ),
                    );
                }

                if let Ok(slot) = self.rpc_client.wait_for_next_slot().await {
                    if slot - last_slot > 20 {
                        debug!(
                            "Waiting for remote tables {} to match local tables.",
                            table_keys_str
                        );
                    }
                    last_slot = slot;
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
    async fn deactivate_tables(
        rpc_client: &MagicblockRpcClient,
        authority: &Keypair,
        released_tables: &Mutex<Vec<LookupTableRc>>,
    ) {
        for table in released_tables
            .lock()
            .await
            .iter_mut()
            .filter(|x| !x.deactivate_triggered())
        {
            // We don't bubble errors as there is no reasonable way to handle them.
            // Instead the next GC cycle will try again to deactivate the table.
            let _ = table.deactivate(rpc_client, authority).await.inspect_err(
                |err| {
                    error!(
                        "Error deactivating table {}: {:?}",
                        table.table_address(),
                        err
                    )
                },
            );
        }
    }

    /// Closes tables that were previously released and deactivated.
    async fn close_tables(
        rpc_client: &MagicblockRpcClient,
        authority: &Keypair,
        released_tables: &Mutex<Vec<LookupTableRc>>,
    ) {
        let Ok(latest_slot) = rpc_client
            .get_slot()
            .await
            .inspect_err(|err| error!("Error getting latest slot: {:?}", err))
        else {
            return;
        };

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
                    .close(rpc_client, authority, Some(latest_slot))
                    .await
                {
                    Ok(closed) if closed => {
                        closed_tables.push(*deactivated_table.table_address())
                    }
                    Ok(_) => {
                        // Table not ready to be closed
                    }
                    Err(err) => error!(
                        "Error closing table {}: {:?}",
                        deactivated_table.table_address(),
                        err
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
