use sleipnir_program::sleipnir_instruction::AccountModification;
use solana_sdk::pubkey::Pubkey;

use crate::{
    errors::MutatorResult, fetch::fetch_account_from_cluster, Cluster,
};

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
) -> MutatorResult<Option<AccountModification>> {
    // First check if we can find an anchor IDL
    match get_pubkey_anchor_idl(program_pubkey) {
        Some(anchor_idl_pubkey) => {
            let anchor_idl_account =
                fetch_account_from_cluster(cluster, &anchor_idl_pubkey).await?;
            return Ok(Some(AccountModification::from((
                &anchor_idl_pubkey,
                &anchor_idl_account,
            ))));
        }
        None => {}
    }
    match get_pubkey_shank_idl(program_pubkey) {
        Some(shank_idl_pubkey) => {
            let shank_idl_account =
                fetch_account_from_cluster(cluster, &shank_idl_pubkey).await?;
            return Ok(Some(AccountModification::from((
                &shank_idl_pubkey,
                &shank_idl_account,
            ))));
        }
        None => {}
    }
    // Otherwise give up
    Ok(None)
}
