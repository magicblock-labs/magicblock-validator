use std::{error::Error, path::PathBuf};

use log::*;
use solana_account::{AccountSharedData, WritableAccount};
use solana_program::{
    bpf_loader_upgradeable::{self, UpgradeableLoaderState},
    rent::Rent,
};
use solana_pubkey::Pubkey;

use crate::scheduler::state::TransactionSchedulerState;
const UPGRADEABLE_LOADER_ID: Pubkey = bpf_loader_upgradeable::ID;

impl TransactionSchedulerState {
    /// Loads BPF upgradeable programs from file paths directly into the `AccountsDb`.
    pub fn load_upgradeable_programs(
        &self,
        progs: &[(Pubkey, PathBuf)],
    ) -> Result<(), Box<dyn Error>> {
        debug!("Loading programs from files: {:#?}", progs);
        for (id, path) in progs {
            let elf = std::fs::read(path).map_err(|err| {
                format!("Failed to read program file for {id}: {err}")
            })?;
            self.add_program(id, &elf)?;
        }
        Ok(())
    }

    /// Creates and stores the accounts for a BPF upgradeable program.
    fn add_program(
        &self,
        id: &Pubkey,
        elf: &[u8],
    ) -> Result<(), Box<dyn Error>> {
        let rent = Rent::default();
        let min_balance = |len| rent.minimum_balance(len).max(1);
        let (programdata_address, _) = Pubkey::find_program_address(
            &[id.as_ref()],
            &UPGRADEABLE_LOADER_ID,
        );

        // 1. Create and store the ProgramData account (which holds the ELF).
        let state = UpgradeableLoaderState::ProgramData {
            slot: 0,
            upgrade_authority_address: Some(Pubkey::default()),
        };
        let mut data = bincode::serialize(&state)?;
        data.extend_from_slice(elf);

        let mut data_account = AccountSharedData::new(
            min_balance(data.len()),
            0,
            &UPGRADEABLE_LOADER_ID,
        );
        data_account.set_data(data);
        self.accountsdb
            .insert_account(&programdata_address, &data_account);

        // 2. Create and store the executable Program account.
        let state = UpgradeableLoaderState::Program {
            programdata_address,
        };
        let exec_bytes = bincode::serialize(&state)?;

        let mut exec_account_data = AccountSharedData::new(
            min_balance(exec_bytes.len()),
            0,
            &UPGRADEABLE_LOADER_ID,
        );
        exec_account_data.set_data(exec_bytes);
        exec_account_data.set_executable(true);
        self.accountsdb.insert_account(id, &exec_account_data);

        Ok(())
    }
}
