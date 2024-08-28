use sleipnir_program::sleipnir_instruction::AccountModification;
use solana_sdk::pubkey::Pubkey;

use crate::{
    utils::{fetch_account, get_pubkey_anchor_idl, get_pubkey_shank_idl},
    Cluster,
};

async fn get_program_idl_modification(
    cluster: &Cluster,
    program_pubkey: &Pubkey,
) -> Option<AccountModification> {
    // First check if we can find an anchor IDL
    let anchor_idl_modification = try_create_account_modification_from_pubkey(
        cluster,
        get_pubkey_anchor_idl(program_pubkey),
    )
    .await;
    if anchor_idl_modification.is_some() {
        return anchor_idl_modification;
    }
    // Otherwise try to find a shank IDL
    let shank_idl_modification = try_create_account_modification_from_pubkey(
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

async fn try_create_account_modification_from_pubkey(
    cluster: &Cluster,
    pubkey: Option<Pubkey>,
) -> Option<AccountModification> {
    if let Some(pubkey) = pubkey {
        if let Ok(account) = fetch_account(cluster, &pubkey).await {
            return Some(AccountModification::from((&pubkey, &account)));
        }
    }
    None
}
