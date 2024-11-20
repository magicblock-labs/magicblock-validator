use std::path::Path;

use sleipnir_bank::bank::Bank;
use sleipnir_core::magic_program;
use solana_sdk::{
    account::Account, clock::Epoch, pubkey::Pubkey, signature::Keypair,
    signer::Signer, system_program,
};

use crate::{
    errors::ApiResult,
    ledger::{read_faucet_keypair_from_ledger, write_faucet_keypair_to_ledger},
};

pub(crate) fn fund_account(bank: &Bank, pubkey: &Pubkey, lamports: u64) {
    bank.store_account(
        pubkey,
        &Account {
            lamports,
            data: vec![],
            owner: system_program::id(),
            executable: false,
            rent_epoch: Epoch::MAX,
        },
    );
}

pub(crate) fn fund_account_with_data(
    bank: &Bank,
    pubkey: &Pubkey,
    lamports: u64,
    data: Vec<u8>,
) {
    bank.store_account(
        pubkey,
        &Account {
            lamports,
            data,
            owner: system_program::id(),
            executable: false,
            rent_epoch: Epoch::MAX,
        },
    );
}

pub(crate) fn fund_validator_identity(bank: &Bank, validator_id: &Pubkey) {
    fund_account(bank, validator_id, u64::MAX / 2);
}

/// Funds the faucet account.
/// If the [create_new] is `false` then the faucet keypair will be read from the
/// existing ledger and an error is raised if it is not found.
/// Otherwise, a new faucet keypair will be created and saved to the ledger.
pub(crate) fn funded_faucet(
    bank: &Bank,
    ledger_path: &Path,
    create_new: bool,
) -> ApiResult<Keypair> {
    let faucet_keypair = if create_new {
        let faucet_keypair = Keypair::new();
        write_faucet_keypair_to_ledger(ledger_path, &faucet_keypair)?;
        faucet_keypair
    } else {
        read_faucet_keypair_from_ledger(ledger_path)?
    };

    fund_account(bank, &faucet_keypair.pubkey(), u64::MAX / 2);
    Ok(faucet_keypair)
}

pub(crate) fn fund_magic_context(bank: &Bank) {
    fund_account_with_data(
        bank,
        &magic_program::MAGIC_CONTEXT_PUBKEY,
        u64::MAX,
        vec![0; magic_program::MAGIC_CONTEXT_SIZE],
    );
}
