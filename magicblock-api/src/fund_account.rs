use std::path::Path;

use magicblock_accounts_db::AccountsDb;
use magicblock_bank::bank::Bank;
use magicblock_core::magic_program;
use solana_sdk::{
    account::{Account, AccountSharedData},
    clock::Epoch,
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
    system_program,
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
    accountsdb.insert_account(
        pubkey,
        &AccountSharedData::new(lamports, size, Default::default()),
    );
}

pub(crate) fn fund_validator_identity(
    accountsdb: &AccountsDb,
    validator_id: &Pubkey,
) {
    fund_account(accountsd, validator_id, u64::MAX / 2);
}

/// Funds the faucet account.
/// If the [create_new] is `false` then the faucet keypair will be read from the
/// existing ledger and an error is raised if it is not found.
/// Otherwise, a new faucet keypair will be created and saved to the ledger.
pub(crate) fn funded_faucet(
    bank: &Bank,
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

    fund_account(bank, &faucet_keypair.pubkey(), u64::MAX / 2);
    Ok(faucet_keypair)
}

pub(crate) fn fund_magic_context(bank: &Bank) {
    fund_account_with_data(
        bank,
        &magic_program::MAGIC_CONTEXT_PUBKEY,
        u64::MAX,
        MAGIC_CONTEXT_SIZE,
    );
}
