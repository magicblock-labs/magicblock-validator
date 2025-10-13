use std::path::Path;

use magicblock_accounts_db::AccountsDb;
use magicblock_core::traits::AccountsBank;
use magicblock_magic_program_api as magic_program;
use solana_sdk::{
    account::{AccountSharedData, WritableAccount},
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
};

use crate::{
    errors::ApiResult,
    ledger::{read_faucet_keypair_from_ledger, write_faucet_keypair_to_ledger},
};

pub(crate) fn fund_account(
    accountsdb: &AccountsDb,
    pubkey: &Pubkey,
    lamports: u64,
) {
    fund_account_with_data(accountsdb, pubkey, lamports, 0);
}

pub(crate) fn fund_account_with_data(
    accountsdb: &AccountsDb,
    pubkey: &Pubkey,
    lamports: u64,
    size: usize,
) {
    let account = if let Some(mut acc) = accountsdb.get_account(pubkey) {
        acc.set_lamports(lamports);
        acc.set_data(vec![0; size]);
        acc
    } else {
        AccountSharedData::new(lamports, size, &Default::default())
    };
    accountsdb.insert_account(pubkey, &account);
}

pub(crate) fn init_validator_identity(
    accountsdb: &AccountsDb,
    validator_id: &Pubkey,
) {
    fund_account(accountsdb, validator_id, u64::MAX / 2);
    let mut authority = accountsdb.get_account(validator_id).unwrap();
    authority.as_borrowed_mut().unwrap().set_privileged(true);
    accountsdb.insert_account(validator_id, &authority);
}

/// Funds the faucet account.
/// If the [create_new] is `false` then the faucet keypair will be read from the
/// existing ledger and an error is raised if it is not found.
/// Otherwise, a new faucet keypair will be created and saved to the ledger.
pub(crate) fn funded_faucet(
    accountsdb: &AccountsDb,
    ledger_path: &Path,
) -> ApiResult<Keypair> {
    let faucet_keypair = match read_faucet_keypair_from_ledger(ledger_path) {
        Ok(faucet_keypair) => faucet_keypair,
        Err(_) => {
            let faucet_keypair = Keypair::new();
            write_faucet_keypair_to_ledger(ledger_path, &faucet_keypair)?;
            faucet_keypair
        }
    };

    fund_account(accountsdb, &faucet_keypair.pubkey(), u64::MAX / 2);
    Ok(faucet_keypair)
}

pub(crate) fn fund_magic_context(accountsdb: &AccountsDb) {
    fund_account_with_data(
        accountsdb,
        &magic_program::MAGIC_CONTEXT_PUBKEY,
        u64::MAX,
        magic_program::MAGIC_CONTEXT_SIZE,
    );
    let mut magic_context = accountsdb
        .get_account(&magic_program::MAGIC_CONTEXT_PUBKEY)
        .unwrap();
    magic_context.set_delegated(true);
    accountsdb
        .insert_account(&magic_program::MAGIC_CONTEXT_PUBKEY, &magic_context);
}
