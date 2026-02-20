use std::path::Path;

use magicblock_accounts_db::{traits::AccountsBank, AccountsDb};
use magicblock_magic_program_api as magic_program;
use magicblock_program::MagicContext;
use solana_account::{AccountSharedData, WritableAccount};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_rent::Rent;
use solana_signer::Signer;

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
        acc
    } else {
        AccountSharedData::new(lamports, size, &Default::default())
    };
    let _ = accountsdb.insert_account(pubkey, &account);
}

pub(crate) fn init_validator_identity(
    accountsdb: &AccountsDb,
    validator_id: &Pubkey,
) {
    fund_account(accountsdb, validator_id, u64::MAX / 2);
    let mut authority = accountsdb.get_account(validator_id).unwrap();
    authority.as_borrowed_mut().unwrap().set_privileged(true);
    let _ = accountsdb.insert_account(validator_id, &authority);
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
    const CONTEXT_LAMPORTS: u64 = u64::MAX;

    fund_account_with_data(
        accountsdb,
        &magic_program::MAGIC_CONTEXT_PUBKEY,
        CONTEXT_LAMPORTS,
        MagicContext::SIZE,
    );
    let mut magic_context = accountsdb
        .get_account(&magic_program::MAGIC_CONTEXT_PUBKEY)
        .expect("magic context should have been created");
    magic_context.set_delegated(true);
    magic_context.set_owner(magic_program::ID);

    let _ = accountsdb
        .insert_account(&magic_program::MAGIC_CONTEXT_PUBKEY, &magic_context);
}

pub(crate) fn fund_ephemeral_vault(accountsdb: &AccountsDb) {
    let lamports = Rent::default().minimum_balance(0);
    fund_account(accountsdb, &magic_program::EPHEMERAL_VAULT_PUBKEY, lamports);
    let mut vault = accountsdb
        .get_account(&magic_program::EPHEMERAL_VAULT_PUBKEY)
        .expect("vault should have been created");
    vault.set_ephemeral(true);
    vault.set_owner(magic_program::ID);
    let _ = accountsdb
        .insert_account(&magic_program::EPHEMERAL_VAULT_PUBKEY, &vault);
}
