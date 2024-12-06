use std::{error::Error, io, path::Path};

use log::*;
use solana_sdk::{
    account::{Account, AccountSharedData},
    bpf_loader_upgradeable::{self, UpgradeableLoaderState},
    pubkey::Pubkey,
    rent::Rent,
};

use crate::bank::Bank;

// -----------------
// LoadableProgram
// -----------------
#[derive(Debug)]
pub struct LoadableProgram {
    pub program_id: Pubkey,
    pub loader_id: Pubkey,
    pub full_path: String,
}

impl LoadableProgram {
    pub fn new(
        program_id: Pubkey,
        loader_id: Pubkey,
        full_path: String,
    ) -> Self {
        Self {
            program_id,
            loader_id,
            full_path,
        }
    }
}

impl From<(Pubkey, String)> for LoadableProgram {
    fn from((program_id, full_path): (Pubkey, String)) -> Self {
        Self::new(program_id, bpf_loader_upgradeable::ID, full_path)
    }
}

impl From<(Pubkey, Pubkey, String)> for LoadableProgram {
    fn from(
        (program_id, loader_id, full_path): (Pubkey, Pubkey, String),
    ) -> Self {
        Self::new(program_id, loader_id, full_path)
    }
}

// -----------------
// Methods to add programs to the bank
// -----------------
pub fn load_programs_into_bank(
    bank: &Bank,
    programs: &[(Pubkey, String)],
) -> Result<(), Box<dyn Error>> {
    if programs.is_empty() {
        return Ok(());
    }
    let mut loadables = Vec::new();
    for prog in programs {
        let full_path = Path::new(&prog.1)
            .canonicalize()?
            .to_str()
            .unwrap()
            .to_string();
        loadables.push(LoadableProgram::new(
            prog.0,
            bpf_loader_upgradeable::ID,
            full_path,
        ));
    }

    add_loadables(bank, &loadables)?;

    Ok(())
}

pub fn add_loadables(
    bank: &Bank,
    progs: &[LoadableProgram],
) -> Result<(), io::Error> {
    debug!("Loading programs: {:#?}", progs);

    let progs: Vec<(Pubkey, Pubkey, Vec<u8>)> = progs
        .iter()
        .map(|prog| {
            let full_path = Path::new(&prog.full_path);
            let elf = std::fs::read(full_path)?;
            Ok((prog.program_id, prog.loader_id, elf))
        })
        .collect::<Result<Vec<_>, io::Error>>()?;

    add_programs_vecs(bank, &progs);

    Ok(())
}

pub fn add_programs_bytes(bank: &Bank, progs: &[(Pubkey, Pubkey, &[u8])]) {
    let elf_program_accounts = progs
        .iter()
        .map(|prog| elf_program_account_from(*prog))
        .collect::<Vec<_>>();
    add_programs(bank, &elf_program_accounts);
}

fn add_programs_vecs(bank: &Bank, progs: &[(Pubkey, Pubkey, Vec<u8>)]) {
    let elf_program_accounts = progs
        .iter()
        .map(|(id, loader_id, vec)| {
            elf_program_account_from((*id, *loader_id, vec))
        })
        .collect::<Vec<_>>();
    add_programs(bank, &elf_program_accounts);
}

fn add_programs(bank: &Bank, progs: &[ElfProgramAccount]) {
    for elf_program_account in progs {
        let ElfProgramAccount {
            program_exec,
            program_data,
        } = elf_program_account;
        let (id, data) = program_exec;
        bank.store_account(id, data);

        if let Some((id, data)) = program_data {
            bank.store_account(id, data);
        }
    }
}

struct ElfProgramAccount {
    pub program_exec: (Pubkey, AccountSharedData),
    pub program_data: Option<(Pubkey, AccountSharedData)>,
}

fn elf_program_account_from(
    (program_id, loader_id, elf): (Pubkey, Pubkey, &[u8]),
) -> ElfProgramAccount {
    let rent = Rent::default();

    let mut program_exec_result = None::<(Pubkey, AccountSharedData)>;
    let mut program_data_result = None::<(Pubkey, AccountSharedData)>;

    if loader_id == solana_sdk::bpf_loader_upgradeable::ID {
        let (programdata_address, _) =
            Pubkey::find_program_address(&[program_id.as_ref()], &loader_id);
        let mut program_data =
            bincode::serialize(&UpgradeableLoaderState::ProgramData {
                slot: 0,
                upgrade_authority_address: Some(Pubkey::default()),
            })
            .unwrap();
        program_data.extend_from_slice(elf);

        program_data_result.replace((
            programdata_address,
            AccountSharedData::from(Account {
                lamports: rent.minimum_balance(program_data.len()).max(1),
                data: program_data,
                owner: loader_id,
                executable: false,
                rent_epoch: 0,
            }),
        ));

        let data = bincode::serialize(&UpgradeableLoaderState::Program {
            programdata_address,
        })
        .unwrap();
        program_exec_result.replace((
            program_id,
            AccountSharedData::from(Account {
                lamports: rent.minimum_balance(data.len()).max(1),
                data,
                owner: loader_id,
                executable: true,
                rent_epoch: 0,
            }),
        ));
    } else {
        let data = elf.to_vec();
        program_exec_result.replace((
            program_id,
            AccountSharedData::from(Account {
                lamports: rent.minimum_balance(data.len()).max(1),
                data,
                owner: loader_id,
                executable: true,
                rent_epoch: 0,
            }),
        ));
    };

    ElfProgramAccount {
        program_exec: program_exec_result
            .expect("Should always have an executable account"),
        program_data: program_data_result,
    }
}
