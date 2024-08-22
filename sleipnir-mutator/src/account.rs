use sleipnir_program::sleipnir_instruction::AccountModification;
use solana_sdk::{account::Account, pubkey::Pubkey};

use crate::utils::create_account_modification_from_account;

pub fn resolve_account_modification(
    account_pubkey: &Pubkey,
    account: &Account,
    overrides: Option<AccountModification>,
) -> AccountModification {
    let mut account_modification =
        create_account_modification_from_account(account_pubkey, account);
    if let Some(overrides) = overrides {
        if let Some(lamports) = overrides.lamports {
            account_modification.lamports = Some(lamports);
        }
        if let Some(owner) = &overrides.owner {
            account_modification.owner = Some(*owner);
        }
        if let Some(executable) = overrides.executable {
            account_modification.executable = Some(executable);
        }
        if let Some(data) = &overrides.data {
            account_modification.data = Some(data.clone());
        }
        if let Some(rent_epoch) = overrides.rent_epoch {
            account_modification.rent_epoch = Some(rent_epoch);
        }
    }
    account_modification
}
