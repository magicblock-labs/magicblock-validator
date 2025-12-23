use magicblock_rpc_client::{MagicBlockRpcClientResult, MagicblockRpcClient};
use solana_address_lookup_table_interface::instruction::derive_lookup_table_address;
use solana_clock::Slot;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;

use crate::lookup_table_rc::LookupTableRc;

pub struct FindOpenTablesOutcome {
    pub addresses_searched: Vec<Pubkey>,
    pub tables: Vec<Pubkey>,
}

pub async fn find_open_tables(
    rpc_client: &MagicblockRpcClient,
    authority: &Keypair,
    min_slot: Slot,
    max_slot: Slot,
    sub_slots_per_slot: u64,
) -> MagicBlockRpcClientResult<FindOpenTablesOutcome> {
    let addresses_searched =
        (min_slot..max_slot).fold(Vec::new(), |mut addresses, slot| {
            for sub_slot in 0..sub_slots_per_slot {
                let derived_auth =
                    LookupTableRc::derive_keypair(authority, slot, sub_slot);
                let (table_address, _) =
                    derive_lookup_table_address(&derived_auth.pubkey(), slot);
                addresses.push(table_address);
            }
            addresses
        });

    let mut tables = Vec::new();
    let accounts = rpc_client
        .get_multiple_accounts(&addresses_searched, None)
        .await?;
    for (pubkey, account) in addresses_searched.iter().zip(accounts.iter()) {
        if account.is_some() {
            tables.push(*pubkey);
        }
    }
    Ok(FindOpenTablesOutcome {
        addresses_searched,
        tables,
    })
}
