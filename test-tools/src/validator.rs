use sleipnir_bank::bank::Bank;
use sleipnir_program::{has_validator_authority, set_validator_authority};
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, signature::Keypair, signer::Signer,
};

use crate::account::fund_account;

pub fn ensure_funded_validator_authority(bank: &Bank) {
    if !has_validator_authority() {
        let validator_authority = Keypair::new();
        let validator_id = validator_authority.pubkey();
        set_validator_authority(validator_authority);
        fund_account(bank, &validator_id, LAMPORTS_PER_SOL * 1_000);
    }
}
