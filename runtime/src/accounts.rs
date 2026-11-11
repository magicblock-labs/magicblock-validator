use std::collections::HashMap;

use config::consts::HEALTHCHECK_ACCOUNT_PUBKEY;
use magic_program_api as magic_program;
use program::MagicContext;
use solana_account::{AccountBuilder, AccountMode, AccountSharedData};
use solana_pubkey::Pubkey;
use solana_rent::Rent;

/// Builds validator-controlled accounts that must exist before execution starts.
pub(crate) fn seeds(programs: &HashMap<Pubkey, Vec<u8>>) -> HashMap<Pubkey, AccountSharedData> {
    let mut accounts = HashMap::from([
        (
            magic_program::MAGIC_CONTEXT_PUBKEY,
            AccountBuilder::default()
                .lamports(u64::MAX)
                .data(vec![0; MagicContext::SIZE])
                .owner(magic_program::ID)
                .mode(AccountMode::Delegated)
                .build(),
        ),
        (
            magic_program::MAGIC_ACCOUNT_VAULT_PUBKEY,
            AccountBuilder::default()
                .lamports(Rent::default().minimum_balance(0))
                .owner(magic_program::ID)
                .mode(AccountMode::Magic)
                .build(),
        ),
        (
            HEALTHCHECK_ACCOUNT_PUBKEY,
            AccountBuilder::default()
                .data(vec![0; size_of::<i64>()])
                .owner(v42_calculator_interface::ID)
                .mode(AccountMode::Magic)
                .build(),
        ),
    ]);

    if !programs.contains_key(&v42_calculator_interface::ID) {
        let sentinel = AccountBuilder::default().mode(AccountMode::Runtime);
        accounts.insert(v42_calculator_interface::ID, sentinel.build());
    }

    accounts
}
