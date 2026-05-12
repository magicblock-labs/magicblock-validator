use std::sync::OnceLock;

use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;

static VALIDATOR_AUTHORITY: OnceLock<Keypair> = OnceLock::new();
static VALIDATOR_AUTHORITY_OVERRIDE: OnceLock<Pubkey> = OnceLock::new();

pub fn validator_authority() -> Keypair {
    VALIDATOR_AUTHORITY.wait().insecure_clone()
}

pub fn validator_authority_id() -> Pubkey {
    VALIDATOR_AUTHORITY.wait().pubkey()
}

pub fn init_validator_authority(keypair: Keypair) {
    let _ = VALIDATOR_AUTHORITY.set(keypair);
}

pub fn set_validator_authority_override(pubkey: Pubkey) {
    let _ = VALIDATOR_AUTHORITY_OVERRIDE.set(pubkey);
}

pub fn validator_authority_override() -> Option<Pubkey> {
    VALIDATOR_AUTHORITY_OVERRIDE.get().copied()
}

pub fn effective_validator_authority_id() -> Pubkey {
    validator_authority_override().unwrap_or_else(validator_authority_id)
}

pub fn generate_validator_authority_if_needed() {
    VALIDATOR_AUTHORITY.get_or_init(Keypair::new);
}
