// NOTE: copied from  runtime/src/bank/address_lookup_table.rs
use solana_sdk::{
    account::{AccountSharedData, ReadableAccount},
    address_lookup_table::{self, state::AddressLookupTable},
    message::{
        v0::{LoadedAddresses, MessageAddressTableLookup},
        AddressLoaderError,
    },
    slot_hashes::SlotHashes,
    transaction::AddressLoader,
};

use super::bank::Bank;

impl AddressLoader for &Bank {
    fn load_addresses(
        self,
        address_table_lookups: &[MessageAddressTableLookup],
    ) -> Result<LoadedAddresses, AddressLoaderError> {
        let slot_hashes = self
            .transaction_processor
            .read()
            .unwrap()
            .sysvar_cache()
            .get_slot_hashes()
            .map_err(|_| AddressLoaderError::SlotHashesSysvarNotFound)?;

        address_table_lookups
            .iter()
            .map(|table| self.load_lookup_table_addresses(table, &slot_hashes))
            .collect::<Result<_, AddressLoaderError>>()
    }
}

impl Bank {
    fn load_lookup_table_addresses(
        &self,
        table: &MessageAddressTableLookup,
        slot_hashes: &SlotHashes,
    ) -> Result<LoadedAddresses, AddressLoaderError> {
        let table_account = self
            .accounts_db
            .get_account(&table.account_key)
            .map(AccountSharedData::from)
            .map_err(|_| AddressLoaderError::LookupTableAccountNotFound)?;
        let current_slot = self.slot();

        if table_account.owner() == &address_lookup_table::program::id() {
            let lookup_table = AddressLookupTable::deserialize(
                table_account.data(),
            )
            .map_err(|_ix_err| AddressLoaderError::InvalidAccountData)?;

            Ok(LoadedAddresses {
                writable: lookup_table
                    .lookup(current_slot, &table.writable_indexes, slot_hashes)
                    .map_err(|_| {
                        AddressLoaderError::LookupTableAccountNotFound
                    })?,
                readonly: lookup_table
                    .lookup(current_slot, &table.readonly_indexes, slot_hashes)
                    .map_err(|_| {
                        AddressLoaderError::LookupTableAccountNotFound
                    })?,
            })
        } else {
            Err(AddressLoaderError::InvalidAccountOwner)
        }
    }
}
