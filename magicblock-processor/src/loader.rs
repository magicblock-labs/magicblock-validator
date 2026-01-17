use std::{error::Error, path::PathBuf};

use magicblock_accounts_db::AccountsDb;
use solana_account::{AccountSharedData, WritableAccount};
use solana_program::{
    bpf_loader_upgradeable::{self, UpgradeableLoaderState},
    rent::Rent,
};
use solana_pubkey::Pubkey;
use tracing::debug;

const UPGRADEABLE_LOADER_ID: Pubkey = bpf_loader_upgradeable::ID;

/// Loads BPF upgradeable programs from file paths directly into the `AccountsDb`.
pub fn load_upgradeable_programs(
    accountsdb: &AccountsDb,
    progs: &[(Pubkey, PathBuf)],
) -> Result<(), Box<dyn Error>> {
    debug!(programs = ?progs, "Loading programs");
    for (id, path) in progs {
        let elf = std::fs::read(path).map_err(|err| {
            format!("Failed to read program file for {id} at {path:?}: {err}")
        })?;
        add_program(accountsdb, id, &elf)?;
    }
    Ok(())
}

/// Creates and stores the accounts for a BPF upgradeable program.
fn add_program(
    accountsdb: &AccountsDb,
    id: &Pubkey,
    elf: &[u8],
) -> Result<(), Box<dyn Error>> {
    let rent = Rent::default();
    let min_balance = |len| rent.minimum_balance(len).max(1);

    // 1. Create and store the ProgramData account (header + ELF)
    let (programdata_address, _) =
        Pubkey::find_program_address(&[id.as_ref()], &UPGRADEABLE_LOADER_ID);

    let mut data = bincode::serialize(&UpgradeableLoaderState::ProgramData {
        slot: 0,
        upgrade_authority_address: Some(Pubkey::default()),
    })?;
    data.extend_from_slice(elf);

    let mut account = AccountSharedData::new(
        min_balance(data.len()),
        0,
        &UPGRADEABLE_LOADER_ID,
    );
    account.set_data(data);
    let _ = accountsdb.insert_account(&programdata_address, &account);

    // 2. Create and store the executable Program account
    let data = bincode::serialize(&UpgradeableLoaderState::Program {
        programdata_address,
    })?;

    let mut account = AccountSharedData::new(
        min_balance(data.len()),
        0,
        &UPGRADEABLE_LOADER_ID,
    );
    account.set_data(data);
    account.set_executable(true);
    let _ = accountsdb.insert_account(id, &account);

    Ok(())
}
