use std::{
    collections::{HashMap, HashSet},
    fmt,
    ops::Deref,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard,
    },
};

use log::*;
use magicblock_metrics::metrics;
use magicblock_rpc_client::{
    MagicBlockRpcClientError, MagicBlockSendTransactionConfig,
    MagicblockRpcClient,
};
use solana_address_lookup_table_interface::{
    self as alt,
    state::{LookupTableMeta, LOOKUP_TABLE_MAX_ADDRESSES},
};
use solana_clock::Slot;
use solana_commitment_config::CommitmentLevel;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_signer::Signer;
use solana_slot_hashes::MAX_ENTRIES;
use solana_transaction::Transaction;

use crate::{
    derive_keypair,
    error::{TableManiaError, TableManiaResult},
    TableManiaComputeBudget,
};

// -----------------
// RefcountedPubkeys
// -----------------

/// A map of reference counted pubkeys that can be used to track the number of
/// reservations that exist for a pubkey in a lookup table
pub struct RefcountedPubkeys {
    pubkeys: HashMap<Pubkey, AtomicUsize>,
}

impl RefcountedPubkeys {
    fn new(pubkeys: &[Pubkey]) -> Self {
        Self {
            pubkeys: pubkeys
                .iter()
                .map(|pubkey| (*pubkey, AtomicUsize::new(1)))
                .collect(),
        }
    }

    /// This should only be called for pubkeys that are not already in this table.
    /// It is called when extending a lookup table with pubkeys that were not
    /// found in any other table.
    fn insert_many(&mut self, pubkeys: &[Pubkey]) {
        for pubkey in pubkeys {
            if let Some(pubkey_rc) = self.pubkeys.get_mut(pubkey) {
                debug!(
                    "Pubkey {} exists in the table. Not inserting, but increasing ref count instead",
                    pubkey
                );
                pubkey_rc.fetch_add(1, Ordering::Relaxed);
            } else {
                self.pubkeys.insert(*pubkey, AtomicUsize::new(1));
            }
        }
    }

    /// Add a reservation to the pubkey if it is part of this table
    /// - *pubkey* to reserve
    /// - *returns* `true` if the pubkey could be reserved
    fn reserve(&self, pubkey: &Pubkey) -> bool {
        if let Some(count) = self.pubkeys.get(pubkey) {
            count.fetch_add(1, Ordering::SeqCst);
            true
        } else {
            false
        }
    }

    /// Called when we are done with a pubkey
    /// Will decrement the ref count of it or do nothing if the pubkey was
    /// not found
    /// - *pubkey* to release
    /// - *returns* `true` if the pubkey was released
    fn release(&self, pubkey: &Pubkey) -> bool {
        if let Some(count) = self.pubkeys.get(pubkey) {
            count
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |x| {
                    if x == 0 {
                        None
                    } else {
                        Some(x - 1)
                    }
                })
                .is_ok()
        } else {
            false
        }
    }

    /// Returns `true` if any of the pubkeys is still in use
    fn has_reservations(&self) -> bool {
        self.pubkeys
            .values()
            .any(|rc_pubkey| rc_pubkey.load(Ordering::SeqCst) > 0)
    }

    /// Returns the refcount of a pubkey if it exists in this table
    /// - *pubkey* to query refcount for
    /// - *returns* `Some(refcount)` if the pubkey exists, `None` otherwise
    fn get_refcount(&self, pubkey: &Pubkey) -> Option<usize> {
        self.pubkeys
            .get(pubkey)
            .map(|count| count.load(Ordering::Relaxed))
    }
}

impl Deref for RefcountedPubkeys {
    type Target = HashMap<Pubkey, AtomicUsize>;

    fn deref(&self) -> &Self::Target {
        &self.pubkeys
    }
}

/// Determined via trial and error, last updated when we added compute budget instructions.
/// The keys themselves take up 24 * 32 = 768 bytes
pub const MAX_ENTRIES_AS_PART_OF_EXTEND: u64 = 24;

// -----------------
// LookupTableRc
// -----------------
pub enum LookupTableRc {
    Active {
        derived_auth: Keypair,
        table_address: Pubkey,
        /// Reference counted pubkeys stored inside the [Self::table].
        /// When someone _checks out_ a pubkey the ref count is incremented
        /// When it is _returned_ the ref count is decremented.
        /// When all pubkeys have ref count 0 the table can be deactivated
        pubkeys: RwLock<RefcountedPubkeys>,
        creation_slot: u64,
        creation_sub_slot: u64,
        init_signature: Signature,
        extend_signatures: Mutex<Vec<Signature>>,
    },
    Deactivated {
        derived_auth: Keypair,
        table_address: Pubkey,
        deactivation_slot: u64,
        deactivate_signature: Signature,
    },
}

impl fmt::Display for LookupTableRc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Active {
                derived_auth,
                table_address,
                pubkeys,
                creation_slot,
                creation_sub_slot,
                init_signature,
                extend_signatures,
            } => {
                let comma_separated_pubkeys = pubkeys
                    .read()
                    .expect("pubkeys rwlock poisoned")
                    .keys()
                    .map(|key| key.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                let comma_separated_sigs = extend_signatures
                    .lock()
                    .expect("extend_signatures mutex poisoned")
                    .iter()
                    .map(|x| x.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(
                    f,
                    "LookupTable: Active {{
    derived_auth: {}
    table_address: {}
    pubkeys: {}
    creation_slot: {}
    creation_sub_slot: {}
    init_signature: {}
    extend_signatures: {}
}}",
                    derived_auth.pubkey(),
                    table_address,
                    comma_separated_pubkeys,
                    creation_slot,
                    creation_sub_slot,
                    init_signature,
                    comma_separated_sigs
                )
            }
            Self::Deactivated {
                derived_auth,
                table_address,
                deactivation_slot,
                deactivate_signature,
            } => {
                write!(
                    f,
                    "LookupTable: Deactivated {{ derived_auth: {}, table_address: {}, deactivation_slot: {}, deactivate_signature: {} }}",
                    derived_auth.pubkey(),
                    table_address,
                    deactivation_slot,
                    deactivate_signature,
                )
            }
        }
    }
}

impl LookupTableRc {
    pub fn init_signature(&self) -> Option<Signature> {
        match self {
            Self::Active { init_signature, .. } => Some(*init_signature),
            Self::Deactivated { .. } => None,
        }
    }

    pub fn extend_signatures(&self) -> Option<Vec<Signature>> {
        match self {
            Self::Active {
                extend_signatures, ..
            } => Some(
                extend_signatures
                    .lock()
                    .expect("extend_signatures mutex poisoned")
                    .clone(),
            ),
            Self::Deactivated { .. } => None,
        }
    }

    pub fn deactivate_signature(&self) -> Option<Signature> {
        match self {
            Self::Active { .. } => None,
            Self::Deactivated {
                deactivate_signature,
                ..
            } => Some(*deactivate_signature),
        }
    }

    pub fn derived_auth(&self) -> &Keypair {
        match self {
            Self::Active { derived_auth, .. } => derived_auth,
            Self::Deactivated { derived_auth, .. } => derived_auth,
        }
    }

    pub fn table_address(&self) -> &Pubkey {
        match self {
            Self::Active { table_address, .. } => table_address,
            Self::Deactivated { table_address, .. } => table_address,
        }
    }

    pub fn pubkeys(&self) -> Option<RwLockReadGuard<'_, RefcountedPubkeys>> {
        match self {
            Self::Active { pubkeys, .. } => {
                Some(pubkeys.read().expect("pubkeys rwlock poisoned"))
            }
            Self::Deactivated { .. } => None,
        }
    }

    pub fn pubkeys_mut(
        &self,
    ) -> Option<RwLockWriteGuard<'_, RefcountedPubkeys>> {
        match self {
            Self::Active { pubkeys, .. } => {
                Some(pubkeys.write().expect("pubkeys rwlock poisoned"))
            }
            Self::Deactivated { .. } => None,
        }
    }

    pub fn creation_slot(&self) -> Option<u64> {
        match self {
            Self::Active { creation_slot, .. } => Some(*creation_slot),
            Self::Deactivated { .. } => None,
        }
    }

    /// Returns `true` if the table has more capacity to add pubkeys
    pub fn has_more_capacity(&self) -> bool {
        self.pubkeys()
            .is_some_and(|x| x.len() < LOOKUP_TABLE_MAX_ADDRESSES)
    }

    pub fn is_full(&self) -> bool {
        !self.has_more_capacity()
    }

    pub fn contains_key(&self, pubkey: &Pubkey) -> bool {
        self.pubkeys()
            .is_some_and(|pubkeys| pubkeys.contains_key(pubkey))
    }

    /// Returns `true` if the table is active and  any of the its pubkeys
    /// is still in use
    pub fn has_reservations(&self) -> bool {
        self.pubkeys()
            .is_some_and(|pubkeys| pubkeys.has_reservations())
    }

    pub fn provides(&self, pubkey: &Pubkey) -> bool {
        self.pubkeys().is_some_and(|pubkeys| {
            pubkeys
                .get(pubkey)
                .is_some_and(|count| count.load(Ordering::SeqCst) > 0)
        })
    }

    /// Returns the refcount of a pubkey if it exists in this table
    /// - *pubkey* to query refcount for
    /// - *returns* `Some(refcount)` if the pubkey exists, `None` otherwise
    pub fn get_refcount(&self, pubkey: &Pubkey) -> Option<usize> {
        self.pubkeys()?.get_refcount(pubkey)
    }
    /// Returns `true` if the we requested to deactivate this table.
    /// NOTE: this doesn't mean that the deactivation period passed, thus
    ///       the table could still be considered _deactivating_ on chain.
    pub fn deactivate_triggered(&self) -> bool {
        use LookupTableRc::*;
        matches!(self, Deactivated { .. })
    }

    pub fn is_active(&self) -> bool {
        use LookupTableRc::*;
        matches!(self, Active { .. })
    }

    pub fn derive_keypair(
        authority: &Keypair,
        slot: Slot,
        sub_slot: Slot,
    ) -> Keypair {
        derive_keypair::derive_keypair(authority, slot, sub_slot)
    }

    /// Reserves the pubkey if it is part of this table.
    /// - *pubkey* to reserve
    /// - *returns* `true` if the pubkey could be reserved
    pub fn reserve_pubkey(&self, pubkey: &Pubkey) -> bool {
        self.pubkeys()
            .is_some_and(|pubkeys| pubkeys.reserve(pubkey))
    }

    /// Releases one reservation for the given pubkey if it is part of this table
    /// and has at least one reservation.
    /// - *pubkey* to release
    /// - *returns* `true` if the pubkey was released
    pub fn release_pubkey(&self, pubkey: &Pubkey) -> bool {
        self.pubkeys()
            .is_some_and(|pubkeys| pubkeys.release(pubkey))
    }

    /// Matches pubkeys from the given set against the pubkeys it has reserved.
    /// NOTE: the caller is responsible to hold a reservation to each pubkey it
    ///       is requesting to match against
    pub fn match_pubkeys(
        &self,
        requested_pubkeys: &HashSet<Pubkey>,
    ) -> HashSet<Pubkey> {
        match self.pubkeys() {
            Some(pubkeys) => requested_pubkeys
                .iter()
                .filter(|pubkey| pubkeys.contains_key(pubkey))
                .cloned()
                .collect::<HashSet<_>>(),
            None => HashSet::new(),
        }
    }

    /// Initializes an address lookup table deriving its authority from the provided
    /// [authority] keypair. The table is extended with the provided [pubkeys].
    /// The [authority] keypair pays for the transaction.
    ///
    /// It is expectected that the provided pubkeys were not found in any other lookup
    /// table nor in this one.
    /// They are automatically reserved for one requestor.
    ///
    /// - **rpc_client**: RPC client to use for sending transactions
    /// - **authority**: Keypair to derive the authority of the lookup table
    /// - **latest_slot**: the on chain slot at which we are creating the table
    /// - **sub_slot**: a bump to allow creating multiple lookup tables with the same authority
    ///   the same slot
    /// - **pubkeys**: to extend the lookup table respecting respecting
    ///   [solana_address_lookup_table_interface::LOOKUP_TABLE_MAX_ADDRESSES]
    ///   after it is initialized
    pub async fn init(
        rpc_client: &MagicblockRpcClient,
        authority: &Keypair,
        latest_slot: Slot,
        sub_slot: Slot,
        pubkeys: &[Pubkey],
        compute_budget: &TableManiaComputeBudget,
    ) -> TableManiaResult<Self> {
        check_max_pubkeys(pubkeys)?;

        let derived_auth =
            Self::derive_keypair(authority, latest_slot, sub_slot);

        let (create_ix, table_address) = alt::instruction::create_lookup_table(
            derived_auth.pubkey(),
            authority.pubkey(),
            latest_slot,
        );
        trace!("Initializing lookup table {}", table_address);

        let end = pubkeys.len().min(LOOKUP_TABLE_MAX_ADDRESSES);
        let extend_ix = alt::instruction::extend_lookup_table(
            table_address,
            derived_auth.pubkey(),
            Some(authority.pubkey()),
            pubkeys[..end].to_vec(),
        );

        let (compute_budget_ix, compute_unit_price_ix) =
            compute_budget.instructions();
        let ixs = vec![
            compute_budget_ix,
            compute_unit_price_ix,
            create_ix,
            extend_ix,
        ];
        let latest_blockhash = rpc_client.get_latest_blockhash().await?;
        let tx = Transaction::new_signed_with_payer(
            &ixs,
            Some(&authority.pubkey()),
            &[authority, &derived_auth],
            latest_blockhash,
        );

        let outcome = rpc_client
            .send_transaction(
                &tx,
                &Self::get_send_transaction_config(rpc_client),
            )
            .await?;
        let (signature, error) = outcome.into_signature_and_error();
        if let Some(error) = &error {
            error!(
                "Error initializing lookup table: {:?} ({})",
                error, signature
            );
            return Err(MagicBlockRpcClientError::SentTransactionError(
                error.clone(),
                signature,
            )
            .into());
        }

        Ok(Self::Active {
            derived_auth,
            table_address,
            pubkeys: RwLock::new(RefcountedPubkeys::new(pubkeys)),
            creation_slot: latest_slot,
            creation_sub_slot: sub_slot,
            init_signature: signature,
            extend_signatures: vec![].into(),
        })
    }

    /// Extends this lookup table with the provided [pubkeys].
    /// The transaction is signed with the [Self::derived_auth].
    ///
    /// It is expectected that the provided pubkeys were not found in any other lookup
    /// table nor in this one.
    /// They are automatically reserved for one requestor.
    ///
    /// - **rpc_client**: RPC client to use for sending the extend transaction
    /// - **authority**:  payer for the extend transaction
    /// - **pubkeys**:    to extend the lookup table with
    pub async fn extend(
        &self,
        rpc_client: &MagicblockRpcClient,
        authority: &Keypair,
        extra_pubkeys: &[Pubkey],
        compute_budget: &TableManiaComputeBudget,
    ) -> TableManiaResult<()> {
        use LookupTableRc::*;

        check_max_pubkeys(extra_pubkeys)?;

        let (pubkeys, extend_signatures) = match self {
            Active {
                pubkeys,
                extend_signatures,
                ..
            } => (pubkeys, extend_signatures),
            Deactivated { .. } => {
                return Err(TableManiaError::CannotExtendDeactivatedTable(
                    *self.table_address(),
                ));
            }
        };
        let (compute_budget_ix, compute_unit_price_ix) =
            compute_budget.instructions();
        let extend_ix = alt::instruction::extend_lookup_table(
            *self.table_address(),
            self.derived_auth().pubkey(),
            Some(authority.pubkey()),
            extra_pubkeys.to_vec(),
        );

        let ixs = vec![compute_budget_ix, compute_unit_price_ix, extend_ix];
        let latest_blockhash = rpc_client.get_latest_blockhash().await?;
        let tx = Transaction::new_signed_with_payer(
            &ixs,
            Some(&authority.pubkey()),
            &[authority, self.derived_auth()],
            latest_blockhash,
        );

        let outcome = rpc_client
            .send_transaction(
                &tx,
                &Self::get_send_transaction_config(rpc_client),
            )
            .await?;
        let (signature, error) = outcome.into_signature_and_error();
        if let Some(error) = &error {
            error!("Error extending lookup table: {:?} ({})", error, signature);
            return Err(MagicBlockRpcClientError::SentTransactionError(
                error.clone(),
                signature,
            )
            .into());
        } else {
            pubkeys
                .write()
                .expect("pubkeys rwlock poisoned")
                .insert_many(extra_pubkeys);
            extend_signatures
                .lock()
                .expect("extend_signatures mutex poisoned")
                .push(signature);
        }

        Ok(())
    }

    /// Extends this lookup table with the portion of the provided [pubkeys] that
    /// fits into the table respecting [solana_sdk::address_lookup_table::state::LOOKUP_TABLE_MAX_ADDRESSES].
    ///
    /// The transaction is signed with the [Self::derived_auth].
    ///
    /// - **rpc_client**: RPC client to use for sending the extend transaction
    /// - **authority**:  payer for the extend transaction
    /// - **pubkeys**:    to extend the lookup table with
    ///
    /// Returns: the pubkeys that were added to the table
    pub async fn extend_respecting_capacity(
        &self,
        rpc_client: &MagicblockRpcClient,
        authority: &Keypair,
        pubkeys: &[Pubkey],
        compute_budget: &TableManiaComputeBudget,
    ) -> TableManiaResult<Vec<Pubkey>> {
        let Some(len) = self.pubkeys().map(|x| x.len()) else {
            return Err(TableManiaError::CannotExtendDeactivatedTable(
                *self.table_address(),
            ));
        };
        let remaining_capacity = LOOKUP_TABLE_MAX_ADDRESSES.saturating_sub(len);
        if remaining_capacity == 0 {
            return Ok(vec![]);
        }

        let storing = if pubkeys.len() >= remaining_capacity {
            let (storing, _) = pubkeys.split_at(remaining_capacity);
            storing
        } else {
            pubkeys
        };

        let res = self
            .extend(rpc_client, authority, storing, compute_budget)
            .await;
        res.map(|_| storing.to_vec())
    }

    /// Deactivates this lookup table.
    ///
    /// - **rpc_client**: RPC client to use for sending the deactivate transaction
    /// - **authority**:  pays for the deactivate transaction
    pub async fn deactivate(
        &mut self,
        rpc_client: &MagicblockRpcClient,
        authority: &Keypair,
        compute_budget: &TableManiaComputeBudget,
    ) -> TableManiaResult<()> {
        let deactivate_ix = alt::instruction::deactivate_lookup_table(
            *self.table_address(),
            self.derived_auth().pubkey(),
        );

        let (compute_budget_ix, compute_unit_price_ix) =
            compute_budget.instructions();
        let ixs = vec![compute_budget_ix, compute_unit_price_ix, deactivate_ix];
        let latest_blockhash = rpc_client.get_latest_blockhash().await?;
        let tx = Transaction::new_signed_with_payer(
            &ixs,
            Some(&authority.pubkey()),
            &[authority, self.derived_auth()],
            latest_blockhash,
        );

        let outcome = rpc_client
            .send_transaction(
                &tx,
                &Self::get_send_transaction_config(rpc_client),
            )
            .await?;
        let (signature, error) = outcome.into_signature_and_error();
        if let Some(error) = &error {
            error!(
                "Error deactivating lookup table: {:?} ({})",
                error, signature
            );
        }

        let slot = rpc_client.get_slot().await?;
        *self = Self::Deactivated {
            derived_auth: self.derived_auth().insecure_clone(),
            table_address: *self.table_address(),
            deactivation_slot: slot,
            deactivate_signature: signature,
        };

        Ok(())
    }

    /// Checks if this lookup table has been requested to be deactivated.
    /// Does not check if the deactivation period has passed. For that use
    /// [Self::is_deactivated_on_chain] instead.
    pub fn is_deactivated(&self) -> bool {
        use LookupTableRc::*;
        matches!(self, Deactivated { .. })
    }

    /// Checks if this lookup table is deactivated via the following:
    ///
    /// 1. was [Self::deactivate] called
    /// 2. is the [LookupTable::Deactivated::deactivation_slot] far enough in the past
    pub async fn is_deactivated_on_chain(
        &self,
        rpc_client: &MagicblockRpcClient,
        current_slot: Option<Slot>,
    ) -> bool {
        let Self::Deactivated {
            deactivation_slot, ..
        } = self
        else {
            return false;
        };
        let slot = {
            if let Some(slot) = current_slot {
                slot
            } else {
                let Ok(slot) = rpc_client.get_slot().await else {
                    return false;
                };
                slot
            }
        };
        // NOTE: the solana explorer will show an account as _deactivated_ once we deactivate it
        //       even though it is actually _deactivating_
        //       I tried to shorten the wait here but found that this is the minimum time needed
        //       for the table to be considered fully _deactivated_
        let deactivated_slot = deactivation_slot + MAX_ENTRIES as u64;
        trace!(
            "'{}' deactivates in {} slots",
            self.table_address(),
            deactivated_slot.saturating_sub(slot),
        );
        deactivated_slot <= slot
    }

    pub async fn is_closed(
        &self,
        rpc_client: &MagicblockRpcClient,
    ) -> TableManiaResult<bool> {
        metrics::inc_table_mania_close_a_count();
        let acc = rpc_client.get_account(self.table_address()).await?;
        Ok(acc.is_none())
    }

    /// Checks if the table was deactivated and if so closes the table account.
    ///
    /// - **rpc_client**: RPC client to use for sending the close transaction
    /// - **authority**:  pays for the close transaction and is refunded the
    ///   table account rent
    /// - **current_slot**: the current slot to use for checking deactivation
    pub async fn close(
        &self,
        rpc_client: &MagicblockRpcClient,
        authority: &Keypair,
        current_slot: Option<Slot>,
        compute_budget: &TableManiaComputeBudget,
    ) -> TableManiaResult<(bool, Option<Signature>)> {
        if !self.is_deactivated_on_chain(rpc_client, current_slot).await {
            return Ok((false, None));
        }

        let close_ix = alt::instruction::close_lookup_table(
            *self.table_address(),
            self.derived_auth().pubkey(),
            authority.pubkey(),
        );

        let (compute_budget_ix, compute_unit_price_ix) =
            compute_budget.instructions();
        let ixs = vec![compute_budget_ix, compute_unit_price_ix, close_ix];
        let latest_blockhash = rpc_client.get_latest_blockhash().await?;
        let tx = Transaction::new_signed_with_payer(
            &ixs,
            Some(&authority.pubkey()),
            &[authority, self.derived_auth()],
            latest_blockhash,
        );

        let outcome = rpc_client
            .send_transaction(
                &tx,
                &Self::get_send_transaction_config(rpc_client),
            )
            .await?;

        let (signature, error) = outcome.into_signature_and_error();
        if let Some(error) = &error {
            debug!(
                "Error closing lookup table: {:?} ({}) - may need longer deactivation time",
                error, signature
            );
        }
        let is_closed = self.is_closed(rpc_client).await?;
        Ok((is_closed, Some(signature)))
    }

    pub async fn get_meta(
        &self,
        rpc_client: &MagicblockRpcClient,
    ) -> TableManiaResult<Option<LookupTableMeta>> {
        Ok(rpc_client
            .get_lookup_table_meta(self.table_address())
            .await?)
    }

    pub async fn get_chain_pubkeys(
        &self,
        rpc_client: &MagicblockRpcClient,
    ) -> TableManiaResult<Option<Vec<Pubkey>>> {
        Self::get_chain_pubkeys_for(rpc_client, self.table_address()).await
    }

    pub async fn get_chain_pubkeys_for(
        rpc_client: &MagicblockRpcClient,
        table_address: &Pubkey,
    ) -> TableManiaResult<Option<Vec<Pubkey>>> {
        Ok(rpc_client.get_lookup_table_addresses(table_address).await?)
    }

    fn get_send_transaction_config(
        rpc_client: &MagicblockRpcClient,
    ) -> MagicBlockSendTransactionConfig {
        use CommitmentLevel::*;
        match rpc_client.commitment_level() {
            Processed => MagicBlockSendTransactionConfig::ensure_processed(),
            Confirmed | Finalized => {
                MagicBlockSendTransactionConfig::ensure_committed()
            }
        }
    }
}

fn check_max_pubkeys(pubkeys: &[Pubkey]) -> TableManiaResult<()> {
    if pubkeys.len() > MAX_ENTRIES_AS_PART_OF_EXTEND as usize {
        return Err(TableManiaError::MaxExtendPubkeysExceeded(
            MAX_ENTRIES_AS_PART_OF_EXTEND as usize,
            pubkeys.len(),
        ));
    }
    Ok(())
}
