use magicblock_program::magicblock_instruction::AccountModification;
use solana_sdk::pubkey::Pubkey;

use crate::{fetch::fetch_account_from_cluster, Cluster};

const ANCHOR_SEED: &str = "anchor:idl";
const SHANK_SEED: &str = "shank:idl";

pub fn get_pubkey_anchor_idl(program_id: &Pubkey) -> Option<Pubkey> {
    let (base, _) = Pubkey::find_program_address(&[], program_id);
    Pubkey::create_with_seed(&base, ANCHOR_SEED, program_id).ok()
}

pub fn get_pubkey_shank_idl(program_id: &Pubkey) -> Option<Pubkey> {
    let (base, _) = Pubkey::find_program_address(&[], program_id);
    Pubkey::create_with_seed(&base, SHANK_SEED, program_id).ok()
}

pub async fn fetch_program_idl_modification_from_cluster(
    cluster: &Cluster,
    program_pubkey: &Pubkey,
) -> Option<AccountModification> {
    // First check if we can find an anchor IDL
    let anchor_idl_modification =
        try_fetch_program_idl_modification_from_cluster(
            cluster,
            get_pubkey_anchor_idl(program_pubkey),
        )
        .await;
    if anchor_idl_modification.is_some() {
        return anchor_idl_modification;
    }
    // Otherwise try to find a shank IDL
    let shank_idl_modification =
        try_fetch_program_idl_modification_from_cluster(
            cluster,
            get_pubkey_shank_idl(program_pubkey),
        )
        .await;
    if shank_idl_modification.is_some() {
        return shank_idl_modification;
    }
    // Otherwise give up
    None
}

async fn try_fetch_program_idl_modification_from_cluster(
    cluster: &Cluster,
    pubkey: Option<Pubkey>,
) -> Option<AccountModification> {
    if let Some(pubkey) = pubkey {
        if let Ok(account) = fetch_account_from_cluster(cluster, &pubkey).await
        {
            return Some(AccountModification::from((&pubkey, &account)));
        }
    }
    None
}
