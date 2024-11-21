use sleipnir_program::{
    sleipnir_instruction::AccountModification, validator_authority_id,
};
use solana_sdk::{
    account::Account,
    bpf_loader_upgradeable::{self, UpgradeableLoaderState},
    clock::Slot,
    pubkey::Pubkey,
    rent::Rent,
    signature::Keypair,
    signer::Signer,
};

use crate::errors::{MutatorModificationError, MutatorModificationResult};

pub struct ProgramModifications {
    pub program_id_modification: AccountModification,
    pub program_data_modification: AccountModification,
    pub program_buffer_modification: AccountModification,
}

pub fn create_program_modifications(
    program_id_pubkey: &Pubkey,
    program_id_account: &Account,
    program_data_pubkey: &Pubkey,
    program_data_account: &Account,
    slot: Slot,
) -> MutatorModificationResult<ProgramModifications> {
    // If we didn't find it then something is off and cloning the program
    // account won't make sense either
    if program_data_account.lamports == 0 {
        return Err(
            MutatorModificationError::CouldNotFindExecutableDataAccount(
                *program_data_pubkey,
                *program_id_pubkey,
            ),
        );
    }
    // If we are not able to find the bytecode from the account, abort
    let program_data_bytecode_index =
        UpgradeableLoaderState::size_of_programdata_metadata();
    if program_data_account.data.len() < program_data_bytecode_index {
        return Err(MutatorModificationError::InvalidProgramDataContent(
            *program_data_pubkey,
            *program_id_pubkey,
        ));
    }
    let program_data_bytecode =
        &program_data_account.data[program_data_bytecode_index..];
    // We'll need to edit the main program account
    let program_id_modification =
        AccountModification::from((program_id_pubkey, program_id_account));
    // Build the proper program_data that we will want to upgrade later
    let program_data_modification = create_program_data_modification(
        program_data_pubkey,
        program_data_bytecode,
        slot,
    );
    // We need to create the upgrade buffer we will use for the bpf_loader transaction later
    let program_buffer_modification =
        create_program_buffer_modification(program_data_bytecode);
    // Done
    Ok(ProgramModifications {
        program_id_modification,
        program_data_modification,
        program_buffer_modification,
    })
}

pub fn create_program_data_modification(
    program_data_pubkey: &Pubkey,
    program_data_bytecode: &[u8],
    slot: Slot,
) -> AccountModification {
    let mut program_data_data =
        bincode::serialize(&UpgradeableLoaderState::ProgramData {
            slot: slot.saturating_sub(1),
            upgrade_authority_address: Some(validator_authority_id()),
        })
        .unwrap();
    program_data_data.extend_from_slice(program_data_bytecode);
    AccountModification::from((
        program_data_pubkey,
        &Account {
            lamports: Rent::default()
                .minimum_balance(program_data_data.len())
                .max(1),
            data: program_data_data,
            owner: bpf_loader_upgradeable::id(),
            executable: false,
            rent_epoch: u64::MAX,
        },
    ))
}

pub fn create_program_buffer_modification(
    program_data_bytecode: &[u8],
) -> AccountModification {
    let mut program_buffer_data =
        bincode::serialize(&UpgradeableLoaderState::Buffer {
            authority_address: Some(validator_authority_id()),
        })
        .unwrap();
    program_buffer_data.extend_from_slice(program_data_bytecode);
    AccountModification::from((
        &Keypair::new().pubkey(),
        &Account {
            lamports: Rent::default()
                .minimum_balance(program_buffer_data.len())
                .max(1),
            data: program_buffer_data,
            owner: bpf_loader_upgradeable::id(),
            executable: false,
            rent_epoch: u64::MAX,
        },
    ))
}
