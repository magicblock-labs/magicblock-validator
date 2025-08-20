use std::error::Error;

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
        progs: &[(Pubkey, String)],
    ) -> Result<(), Box<dyn Error>> {
        debug!("Loading programs from files: {:#?}", progs);
        for (id, path) in progs {
            let elf = std::fs::read(path)?;
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
        let (data_addr, _) = Pubkey::find_program_address(
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

        let data_account = AccountSharedData::new_data(
            min_balance(data.len()),
            &data,
            &UPGRADEABLE_LOADER_ID,
        )?;
        self.accountsdb.insert_account(&data_addr, &data_account);

        // 2. Create and store the executable Program account.
        let exec_bytes =
            bincode::serialize(&UpgradeableLoaderState::Program {
                programdata_address: data_addr,
            })?;

        let mut exec_account_data = AccountSharedData::new_data(
            min_balance(exec_bytes.len()),
            &exec_bytes,
            &UPGRADEABLE_LOADER_ID,
        )?;
        exec_account_data.set_executable(true);
        self.accountsdb.insert_account(id, &exec_account_data);

        Ok(())
    }
}
