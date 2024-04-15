use log::*;
use std::{error::Error, io, path::Path};

use sleipnir_bank::bank::Bank;
use solana_sdk::{
    account::{Account, AccountSharedData},
    bpf_loader_upgradeable::{self, UpgradeableLoaderState},
    pubkey::Pubkey,
    rent::Rent,
};

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
/// Uses the default loader to load programs which need to be provided in
/// a single string as follows:
///
/// ```text
/// "<program_id>:<full_path>,<program_id>:<full_path>,..."
/// ```
pub fn load_programs_from_string_config(
    bank: &Bank,
    programs: &str,
) -> Result<(), Box<dyn Error>> {
    fn extract_program_info_from_parts(
        s: &str,
    ) -> Result<LoadableProgram, Box<dyn Error>> {
        let parts = s.trim().split(':').collect::<Vec<_>>();
        if parts.len() != 2 {
            return Err(format!("Invalid program definition: {}", s).into());
        }
        let program_id = parts[0].parse::<Pubkey>()?;
        let full_path = parts[1].to_string();
        Ok(LoadableProgram::new(
            program_id,
            bpf_loader_upgradeable::ID,
            full_path,
        ))
    }

    let loadables = programs
        .split(',')
        .collect::<Vec<_>>()
        .into_iter()
        .map(extract_program_info_from_parts)
        .collect::<Result<Vec<LoadableProgram>, Box<dyn Error>>>()?;

    add_loadables(bank, &loadables)?;

    Ok(())
}

pub fn add_loadables(
    bank: &Bank,
    progs: &[LoadableProgram],
) -> Result<(), io::Error> {
    debug!("Loading programs: {:#?}", progs);

    let progs: Vec<(Pubkey, Pubkey, Vec<u8>)> = progs
        .into_iter()
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
        .into_iter()
        .map(|prog| elf_program_account_from(*prog))
        .collect::<Vec<_>>();
    add_programs(bank, &elf_program_accounts);
}

fn add_programs_vecs(bank: &Bank, progs: &[(Pubkey, Pubkey, Vec<u8>)]) {
    let elf_program_accounts = progs
        .into_iter()
        .map(|(id, loader_id, vec)| {
            elf_program_account_from((*id, *loader_id, &vec))
        })
        .collect::<Vec<_>>();
    add_programs(bank, &elf_program_accounts);
}

fn add_programs(bank: &Bank, progs: &[ElfProgramAccount]) {
    for elf_program_account in progs.into_iter() {
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
