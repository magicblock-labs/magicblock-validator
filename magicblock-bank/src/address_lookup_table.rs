// NOTE: copied from  runtime/src/bank/address_lookup_table.rs
use solana_sdk::{
    address_lookup_table::error::AddressLookupError,
    message::{
        v0::{LoadedAddresses, MessageAddressTableLookup},
        AddressLoaderError,
    },
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
            .sysvar_cache
            .read()
            .unwrap()
            .get_slot_hashes()
            .map_err(|_| AddressLoaderError::SlotHashesSysvarNotFound)?;

        Ok(address_table_lookups
            .iter()
            .map(|address_table_lookup| {
                self.rc.accounts.load_lookup_table_addresses(
                    self.slot(),
                    address_table_lookup,
                    &slot_hashes,
                )
            })
            .collect::<Result<_, AddressLookupError>>()?)
    }
}
