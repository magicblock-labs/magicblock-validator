use magicblock_chainlink::{
    cloner::errors::{ClonerError, ClonerResult},
    remote_account_provider::program_account::LoadedProgram,
};
use magicblock_magic_program_api::instruction::AccountModification;
use solana_sdk::{
    bpf_loader_upgradeable::{self, UpgradeableLoaderState},
    pubkey::Pubkey,
    rent::Rent,
};

pub struct BpfUpgradableProgramModifications {
    pub program_id_modification: AccountModification,
    pub program_data_modification: AccountModification,
}

fn create_loader_data(loaded_program: &LoadedProgram) -> ClonerResult<Vec<u8>> {
    let loader_state = UpgradeableLoaderState::ProgramData {
        slot: 10,
        upgrade_authority_address: Some(loaded_program.authority),
    };
    let mut loader_data = bincode::serialize(&loader_state)?;
    loader_data.extend_from_slice(&loaded_program.program_data);
    Ok(loader_data)
}

impl TryFrom<&LoadedProgram> for BpfUpgradableProgramModifications {
    type Error = ClonerError;
    fn try_from(loaded_program: &LoadedProgram) -> Result<Self, Self::Error> {
        let (program_data_address, _) = Pubkey::find_program_address(
            &[loaded_program.program_id.as_ref()],
            &bpf_loader_upgradeable::id(),
        );

        // 1. Create and store the ProgramData account (which holds the program data).
        let program_data_modification = {
            let loader_data = create_loader_data(loaded_program)?;
            AccountModification {
                pubkey: program_data_address,
                lamports: Some(
                    Rent::default().minimum_balance(loader_data.len()),
                ),
                data: Some(loader_data),
                owner: Some(bpf_loader_upgradeable::id()),
                executable: Some(false),
                rent_epoch: Some(u64::MAX),
                delegated: Some(false),
            }
        };

        // 2. Create and store the executable Program account.
        let program_id_modification = {
            let state = UpgradeableLoaderState::Program {
                programdata_address: program_data_address,
            };
            let exec_bytes = bincode::serialize(&state)?;
            AccountModification {
                pubkey: loaded_program.program_id,
                lamports: Some(
                    Rent::default().minimum_balance(exec_bytes.len()).max(1),
                ),
                data: Some(exec_bytes),
                owner: Some(bpf_loader_upgradeable::id()),
                executable: Some(true),
                rent_epoch: Some(u64::MAX),
                delegated: Some(false),
            }
        };

        Ok(BpfUpgradableProgramModifications {
            program_id_modification,
            program_data_modification,
        })
    }
}
