use solana_sdk::pubkey::Pubkey;

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
