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

use crate::{
    errors::{MutatorError, MutatorResult},
    utils::{
        fetch_account, get_pubkey_anchor_idl, get_pubkey_program_data,
        get_pubkey_shank_idl,
    },
    Cluster,
};

pub struct ProgramModifications {
    pub program_modification: AccountModification,
    pub program_data_modification: AccountModification,
    pub program_buffer_modification: AccountModification,
    pub program_idl_modification: Option<AccountModification>,
}

pub async fn resolve_program_modifications(
    cluster: &Cluster,
    program_pubkey: &Pubkey,
    program_account: &Account,
    slot: Slot,
) -> MutatorResult<ProgramModifications> {
    // If it's an executable, we will need to modify multiple accounts
    let program_modification =
        AccountModification::from((program_pubkey, program_account));

    // The program data needs to be cloned, download the executable account
    let program_data_pubkey = get_pubkey_program_data(program_pubkey);
    let program_data_account_remote =
        fetch_account(cluster, &program_data_pubkey)
            .await
            .map_err(|err| {
                MutatorError::FailedToCloneProgramExecutableDataAccount(
                    *program_pubkey,
                    err,
                )
            })?;
    // If we didn't find it then something is off and cloning the program
    // account won't make sense either
    if program_data_account_remote.lamports == 0 {
        return Err(MutatorError::CouldNotFindExecutableDataAccount(
            program_data_pubkey,
            *program_pubkey,
        ));
    }
    // If we are not able to find the bytecode from the account, abort
    let program_data_bytecode_index =
        UpgradeableLoaderState::size_of_programdata_metadata();
    if program_data_account_remote.data.len() < program_data_bytecode_index {
        return Err(MutatorError::InvalidProgramDataContent(
            program_data_pubkey,
            *program_pubkey,
        ));
    }

    // Build the proper program_data that we will want to upgrade later
    let mut program_data_data =
        bincode::serialize(&UpgradeableLoaderState::ProgramData {
            slot: slot.saturating_sub(1),
            upgrade_authority_address: Some(validator_authority_id()),
        })
        .unwrap();
    program_data_data.extend_from_slice(
        &program_data_account_remote.data[program_data_bytecode_index..],
    );
    let program_data_account = Account {
        lamports: Rent::default()
            .minimum_balance(program_data_data.len())
            .max(1),
        data: program_data_data,
        owner: bpf_loader_upgradeable::id(),
        executable: false,
        rent_epoch: u64::MAX,
    };
    let program_data_modification = AccountModification::from((
        &program_data_pubkey,
        &program_data_account,
    ));

    // We need to create the upgrade buffer we will use for the bpf_loader transaction later
    let program_buffer_pubkey = Keypair::new().pubkey();
    let mut program_buffer_data =
        bincode::serialize(&UpgradeableLoaderState::Buffer {
            authority_address: Some(validator_authority_id()),
        })
        .unwrap();
    program_buffer_data.extend_from_slice(
        &program_data_account_remote.data[program_data_bytecode_index..],
    );
    let program_buffer_account = Account {
        lamports: Rent::default()
            .minimum_balance(program_buffer_data.len())
            .max(1),
        data: program_buffer_data,
        owner: bpf_loader_upgradeable::id(),
        executable: false,
        rent_epoch: u64::MAX,
    };
    let program_buffer_modification = AccountModification::from((
        &program_buffer_pubkey,
        &program_buffer_account,
    ));

    // Finally try to find the IDL if we can
    let program_idl_modification =
        get_program_idl_modification(cluster, program_pubkey).await;

    // Done
    Ok(ProgramModifications {
        program_modification,
        program_data_modification,
        program_buffer_modification,
        program_idl_modification,
    })
}

async fn get_program_idl_modification(
    cluster: &Cluster,
    program_pubkey: &Pubkey,
) -> Option<AccountModification> {
    // First check if we can find an anchor IDL
    let anchor_idl_modification = try_create_account_modification_from_pubkey(
        cluster,
        get_pubkey_anchor_idl(program_pubkey),
    )
    .await;
    if anchor_idl_modification.is_some() {
        return anchor_idl_modification;
    }
    // Otherwise try to find a shank IDL
    let shank_idl_modification = try_create_account_modification_from_pubkey(
        cluster,
        get_pubkey_shank_idl(program_pubkey),
    )
    .await;
    if shank_idl_modification.is_some() {
        return shank_idl_modification;
    }
    // Otherwise give up
    None
}

async fn try_create_account_modification_from_pubkey(
    cluster: &Cluster,
    pubkey: Option<Pubkey>,
) -> Option<AccountModification> {
    if let Some(pubkey) = pubkey {
        if let Ok(account) = fetch_account(cluster, &pubkey).await {
            return Some(AccountModification::from((&pubkey, &account)));
        }
    }
    None
}
