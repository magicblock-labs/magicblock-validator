// Has the few methods we need from the [chainparser](https://crates.io/crates/chainparser) crate.
// It's easier to duplicate them here instead of dealing with sdk::PublicKey dep issues.
use solana_sdk::pubkey::Pubkey;

const ANCHOR_SEED: &str = "anchor:idl";
const SHANK_SEED: &str = "shank:idl";

/// Resolves the addresses of IDL accounts for `(anchor, shank)`.
pub fn get_idl_addresses(
    program_id: &Pubkey,
) -> (Option<Pubkey>, Option<Pubkey>) {
    let (base, _) = Pubkey::find_program_address(&[], program_id);
    let anchor = Pubkey::create_with_seed(&base, ANCHOR_SEED, program_id).ok();
    let shank = Pubkey::create_with_seed(&base, SHANK_SEED, program_id).ok();
    (anchor, shank)
}
