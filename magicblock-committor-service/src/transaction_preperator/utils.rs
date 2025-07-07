use solana_pubkey::Pubkey;
use solana_sdk::{
    address_lookup_table::state::AddressLookupTable,
    message::AddressLookupTableAccount, transaction::VersionedTransaction,
};

/// Returns [`Vec<AddressLookupTableAccount>`] where all TX accounts stored in ALT
pub fn estimate_lookup_tables_for_tx(
    transaction: &VersionedTransaction,
) -> Vec<AddressLookupTableAccount> {
    transaction
        .message
        .static_account_keys()
        .chunks(256)
        .map(|addresses| AddressLookupTableAccount {
            key: Pubkey::new_unique(),
            addresses: addresses.to_vec(),
        })
        .collect()
}
