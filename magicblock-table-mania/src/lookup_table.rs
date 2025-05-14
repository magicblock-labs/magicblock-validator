use log::*;
use std::fmt;
use std::sync::Mutex;

use crate::derive_keypair;
use crate::error::{TableManiaError, TableManiaResult};
use magicblock_rpc_client::MagicBlockRpcClientError;
use magicblock_rpc_client::{
    MagicBlockSendTransactionConfig, MagicblockRpcClient,
};
use solana_pubkey::Pubkey;
use solana_sdk::address_lookup_table::state::{
    LookupTableMeta, LOOKUP_TABLE_MAX_ADDRESSES,
};
use solana_sdk::commitment_config::CommitmentLevel;
use solana_sdk::slot_hashes::MAX_ENTRIES;
use solana_sdk::{
    address_lookup_table as alt,
    clock::Slot,
    signature::{Keypair, Signature},
    signer::Signer,
    transaction::Transaction,
};

/// Determined via trial and error. The keys themselves take up
/// 27 * 32 bytes = 864 bytes.
pub const MAX_ENTRIES_AS_PART_OF_EXTEND: u64 = 27;

#[derive(Debug)]
pub enum LookupTable {
    Active {
        derived_auth: Keypair,
        table_address: Pubkey,
        pubkeys: Mutex<Vec<Pubkey>>,
        creation_slot: u64,
        creation_sub_slot: u64,
        init_signature: Signature,
        extend_signatures: Vec<Signature>,
    },
    Deactivated {
        derived_auth: Keypair,
        table_address: Pubkey,
        deactivation_slot: u64,
        deactivate_signature: Signature,
    },
}

impl fmt::Display for LookupTable {
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
                    .lock()
                    .expect("pubkeys mutex poisoned")
                    .iter()
                    .map(|x| x.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                let comma_separated_sigs = extend_signatures
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

impl LookupTable {
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

    /// All pubkeys requested, no matter of the `reqid`.
    /// The same pubkey might be included twice if requested with different `reqid`.
    pub fn pubkeys(&self) -> Option<Vec<Pubkey>> {
        match self {
            Self::Active { pubkeys, .. } => {
                Some(pubkeys.lock().expect("pubkeys mutex poisoned").to_vec())
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

    pub fn has_more_capacity(&self) -> bool {
        self.pubkeys()
            .is_some_and(|x| x.len() < LOOKUP_TABLE_MAX_ADDRESSES)
    }

    pub fn contains(&self, pubkey: &Pubkey, _reqid: u64) -> bool {
        match self {
            Self::Active { pubkeys, .. } => pubkeys
                .lock()
                .expect("pubkeys mutex poisoned")
                .contains(pubkey),
            Self::Deactivated { .. } => false,
        }
    }

    /// Returns `true` if the we requested to deactivate this table.
    /// NOTE: this doesn't mean that the deactivation perios passed, thus
    ///       the table could still be considered _deactivating_ on chain.
    pub fn deactivate_triggered(&self) -> bool {
        use LookupTable::*;
        matches!(self, Deactivated { .. })
    }

    pub fn is_active(&self) -> bool {
        use LookupTable::*;
        matches!(self, Active { .. })
    }

    pub fn derive_keypair(
        authority: &Keypair,
        slot: Slot,
        sub_slot: Slot,
    ) -> Keypair {
        derive_keypair::derive_keypair(authority, slot, sub_slot)
    }

    /// Initializes an address lookup table deriving its authority from the provided
    /// [authority] keypair. The table is extended with the provided [pubkeys].
    /// The [authority] keypair pays for the transaction.
    ///
    /// - **rpc_client**: RPC client to use for sending transactions
    /// - **authority**: Keypair to derive the authority of the lookup table
    /// - **latest_slot**: the on chain slot at which we are creating the table
    /// - **sub_slot**: a bump to allow creating multiple lookup tables with the same authority
    ///   at the same slot
    /// - **pubkeys**: to extend the lookup table respecting respecting
    ///   solana_sdk::address_lookup_table::state::LOOKUP_TABLE_MAX_ADDRESSES]
    ///   after it is initialized
    /// - **reqid**:   id of the request adding the pubkeys
    pub async fn init(
        rpc_client: &MagicblockRpcClient,
        authority: &Keypair,
        latest_slot: Slot,
        sub_slot: Slot,
        pubkeys: &[Pubkey],
        _reqid: u64,
    ) -> TableManiaResult<Self> {
        check_max_pubkeys(pubkeys)?;

        let derived_auth =
            Self::derive_keypair(authority, latest_slot, sub_slot);

        let (create_ix, table_address) = alt::instruction::create_lookup_table(
            derived_auth.pubkey(),
            authority.pubkey(),
            latest_slot,
        );

        let end = pubkeys.len().min(LOOKUP_TABLE_MAX_ADDRESSES);
        let extend_ix = alt::instruction::extend_lookup_table(
            table_address,
            derived_auth.pubkey(),
            Some(authority.pubkey()),
            pubkeys[..end].to_vec(),
        );

        let ixs = vec![create_ix, extend_ix];
        let latest_blockhash = rpc_client.get_latest_blockhash().await?;
        let tx = Transaction::new_signed_with_payer(
            &ixs,
            Some(&authority.pubkey()),
            &[authority, &derived_auth],
            latest_blockhash,
        );

        let outcome = rpc_client
            .send_transaction(&tx, &Self::get_commitment(rpc_client))
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
            pubkeys: Mutex::new(pubkeys.to_vec()),
            creation_slot: latest_slot,
            creation_sub_slot: sub_slot,
            init_signature: signature,
            extend_signatures: vec![],
        })
    }

    fn get_commitment(
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

    /// Extends this lookup table with the provided [pubkeys].
    /// The transaction is signed with the [Self::derived_auth].
    ///
    /// - **rpc_client**: RPC client to use for sending the extend transaction
    /// - **authority**:  payer for the the extend transaction
    /// - **pubkeys**:    to extend the lookup table with
    /// - **reqid**:      id of the request adding the pubkeys
    pub async fn extend(
        &self,
        rpc_client: &MagicblockRpcClient,
        authority: &Keypair,
        extra_pubkeys: &[Pubkey],
        _reqid: u64,
    ) -> TableManiaResult<()> {
        use LookupTable::*;

        check_max_pubkeys(extra_pubkeys)?;

        let pubkeys = match self {
            Active { pubkeys, .. } => pubkeys,
            Deactivated { .. } => {
                return Err(TableManiaError::CannotExtendDeactivatedTable(
                    *self.table_address(),
                ));
            }
        };
        let extend_ix = alt::instruction::extend_lookup_table(
            *self.table_address(),
            self.derived_auth().pubkey(),
            Some(authority.pubkey()),
            extra_pubkeys.to_vec(),
        );

        let ixs = vec![extend_ix];
        let latest_blockhash = rpc_client.get_latest_blockhash().await?;
        let tx = Transaction::new_signed_with_payer(
            &ixs,
            Some(&authority.pubkey()),
            &[authority, self.derived_auth()],
            latest_blockhash,
        );

        let outcome = rpc_client
            .send_transaction(&tx, &Self::get_commitment(rpc_client))
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
                .lock()
                .expect("pubkeys mutex poisoned")
                .extend(extra_pubkeys);
        }

        Ok(())
    }

    /// Extends this lookup table with the portion of the provided [pubkeys] that
    /// fits into the table respecting [solana_sdk::address_lookup_table::state::LOOKUP_TABLE_MAX_ADDRESSES].
    ///
    /// The transaction is signed with the [Self::derived_auth].
    ///
    /// - **rpc_client**: RPC client to use for sending the extend transaction
    /// - **authority**:  payer for the the extend transaction
    /// - **pubkeys**:    to extend the lookup table with
    /// - **reqid**:      id of the request adding the pubkeys
    ///
    /// Returns: the pubkeys that were added to the table
    pub async fn extend_respecting_capacity(
        &self,
        rpc_client: &MagicblockRpcClient,
        authority: &Keypair,
        pubkeys: &[Pubkey],
        reqid: u64,
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

        let res = self.extend(rpc_client, authority, storing, reqid).await;
        res.map(|_| storing.to_vec())
    }

    /// Deactivates this lookup table.
    ///
    /// - **rpc_client**: RPC client to use for sending the deactivate transaction
    /// - **authority**:  pays for the the deactivate transaction
    pub async fn deactivate(
        &mut self,
        rpc_client: &MagicblockRpcClient,
        authority: &Keypair,
    ) -> TableManiaResult<()> {
        let deactivate_ix = alt::instruction::deactivate_lookup_table(
            *self.table_address(),
            self.derived_auth().pubkey(),
        );
        let ixs = vec![deactivate_ix];
        let latest_blockhash = rpc_client.get_latest_blockhash().await?;
        let tx = Transaction::new_signed_with_payer(
            &ixs,
            Some(&authority.pubkey()),
            &[authority, self.derived_auth()],
            latest_blockhash,
        );

        let outcome = rpc_client
            .send_transaction(&tx, &Self::get_commitment(rpc_client))
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

    /// Checks if this lookup table is deactivated via the following:
    ///
    /// 1. was [Self::deactivate] called
    /// 2. is the [LookupTable::Deactivated::deactivation_slot] far enough in the past
    pub async fn is_deactivated(
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
        // NOTE: the solana exporer will show an account as _deactivated_ once we deactivate it
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
        let acc = rpc_client.get_account(self.table_address()).await?;
        Ok(acc.is_none())
    }

    /// Checks if the table was deactivated and if so closes the table account.
    ///
    /// - **rpc_client**: RPC client to use for sending the close transaction
    /// - **authority**:  pays for the the close transaction and is refunded the
    ///   table account rent
    /// - **current_slot**: the current slot to use for checking deactivation
    pub async fn close(
        &self,
        rpc_client: &MagicblockRpcClient,
        authority: &Keypair,
        current_slot: Option<Slot>,
    ) -> TableManiaResult<bool> {
        if !self.is_deactivated(rpc_client, current_slot).await {
            return Ok(false);
        }

        let close_ix = alt::instruction::close_lookup_table(
            *self.table_address(),
            self.derived_auth().pubkey(),
            authority.pubkey(),
        );
        let ixs = vec![close_ix];
        let latest_blockhash = rpc_client.get_latest_blockhash().await?;
        let tx = Transaction::new_signed_with_payer(
            &ixs,
            Some(&authority.pubkey()),
            &[authority, self.derived_auth()],
            latest_blockhash,
        );

        let outcome = rpc_client
            .send_transaction(&tx, &Self::get_commitment(rpc_client))
            .await?;

        let (signature, error) = outcome.into_signature_and_error();
        if let Some(error) = &error {
            debug!(
                "Error closing lookup table: {:?} ({}) - may need longer deactivation time",
                error, signature
            );
        }
        self.is_closed(rpc_client).await
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
