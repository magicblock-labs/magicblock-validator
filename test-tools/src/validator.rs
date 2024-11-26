use sleipnir_bank::bank::Bank;
use sleipnir_program::validator;
use solana_sdk::native_token::LAMPORTS_PER_SOL;

use crate::account::fund_account;

pub fn ensure_funded_validator_authority(bank: &Bank) {
    validator::generate_validator_authority_if_needed();
    fund_account(
        bank,
        &validator::validator_authority_id(),
        LAMPORTS_PER_SOL * 1_000,
    );
}
