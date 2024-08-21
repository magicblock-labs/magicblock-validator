use sleipnir_program::sleipnir_instruction::AccountModification;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    account::Account, clock::Slot, commitment_config::CommitmentConfig,
    pubkey::Pubkey, rent::Rent, signature::Keypair, signer::Signer,
};

use crate::{
    adjust_deployment_slot::adjust_deployment_slot,
    errors::{MutatorError, MutatorResult},
    get_pubkey::{
        get_pubkey_anchor_idl, get_pubkey_program_data, get_pubkey_shank_idl,
    },
    Cluster,
};

pub enum CloningAccountModifications {
    Account {
        account_modification: AccountModification,
    },
    Program {
        program_modification: AccountModification,
        program_data_modification: AccountModification,
        program_buffer_modification: AccountModification,
        program_idl_modification: Option<AccountModification>,
    },
}

pub async fn mods_to_clone_account(
    cluster: &Cluster,
    account_pubkey: &Pubkey,
    account: Option<Account>,
    slot: Slot,
    overrides: Option<AccountModification>,
) -> MutatorResult<CloningAccountModifications> {
    // Fetch all accounts to clone

    // 1. Download the account info if needed
    let account = match account {
        Some(account) => account,
        None => get_account(cluster, account_pubkey).await?,
    };

    // If it's a regular account that's not executable, use happy path
    if !account.executable {
        return Ok(CloningAccountModifications::Account {
            account_modification: get_account_modification_with_overrides(
                account_pubkey,
                &account,
                overrides,
            ),
        });
    }

    // If it's an executable, we will need to modify multiple accounts
    let program_pubkey = account_pubkey;
    let program_account = account;
    let program_modification =
        account_modification_from_account(program_pubkey, &program_account);

    // The program data needs to be cloned, download the executable account
    let program_data_pubkey = get_pubkey_program_data(program_pubkey);
    let mut program_data_account = get_account(cluster, &program_data_pubkey)
        .await
        .map_err(|err| {
            MutatorError::FailedToCloneProgramExecutableDataAccount(
                *account_pubkey,
                err,
            )
        })?;
    // If we didn't find it then something is off and cloning the program
    // account won't make sense either
    if program_data_account.lamports == 0 {
        return Err(MutatorError::CouldNotFindExecutableDataAccount(
            program_data_pubkey,
            *account_pubkey,
        ));
    }
    // NOTE: we ran into issues with transactions running right after a program was cloned,
    // i.e. the first transaction using it.
    // In those cases we saw "Program is not deployed" errors which most often showed
    // up during transaction simulations.
    // Claiming that the program was deployed one slot earlier fixed the issue.
    // For more information see: https://github.com/magicblock-labs/magicblock-validator/pull/83
    let targeted_deployment_slot = if slot == 0 { slot } else { slot - 1 };
    adjust_deployment_slot(
        program_pubkey,
        &program_data_pubkey,
        &program_account,
        &mut program_data_account,
        targeted_deployment_slot,
    )?;
    let program_data_modification = account_modification_from_account(
        &program_data_pubkey,
        &program_data_account,
    );

    // We need to create the upgrade buffer we will use for the bpf_loader transaction later
    let program_buffer_pubkey = Keypair::new().pubkey();
    let program_buffer_data =
        Vec::from_iter(program_data_account.data[8..].iter().cloned());
    let program_buffer_account = Account {
        lamports: Rent::default()
            .minimum_balance(program_buffer_data.len())
            .max(1),
        data: program_buffer_data,
        owner: program_data_account.owner,
        executable: false,
        rent_epoch: 0,
    };
    let program_buffer_modification = account_modification_from_account(
        &program_buffer_pubkey,
        &program_buffer_account,
    );

    // Finally try to find the IDL if we can
    let program_idl_modification =
        get_program_idl_modification(cluster, program_pubkey).await;

    // Done
    Ok(CloningAccountModifications::Program {
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
    let anchor_idl_modification = try_get_account_modification_from_pubkey(
        cluster,
        get_pubkey_anchor_idl(program_pubkey),
    )
    .await;
    if anchor_idl_modification.is_some() {
        return anchor_idl_modification;
    }
    // Otherwise try to find a shank IDL
    let shank_idl_modification = try_get_account_modification_from_pubkey(
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

async fn try_get_account_modification_from_pubkey(
    cluster: &Cluster,
    pubkey: Option<Pubkey>,
) -> Option<AccountModification> {
    if let Some(pubkey) = pubkey {
        if let Some(account) = get_account(cluster, &pubkey).await.ok() {
            return Some(account_modification_from_account(&pubkey, &account));
        }
    }
    None
}

fn get_account_modification_with_overrides(
    account_pubkey: &Pubkey,
    account: &Account,
    overrides: Option<AccountModification>,
) -> AccountModification {
    let mut account_mod =
        account_modification_from_account(account_pubkey, &account);
    if let Some(overrides) = overrides {
        if let Some(lamports) = overrides.lamports {
            account_mod.lamports = Some(lamports);
        }
        if let Some(owner) = &overrides.owner {
            account_mod.owner = Some(*owner);
        }
        if let Some(executable) = overrides.executable {
            account_mod.executable = Some(executable);
        }
        if let Some(data) = &overrides.data {
            account_mod.data = Some(data.clone());
        }
        if let Some(rent_epoch) = overrides.rent_epoch {
            account_mod.rent_epoch = Some(rent_epoch);
        }
    }
    account_mod
}

fn account_modification_from_account(
    account_pubkey: &Pubkey,
    account: &Account,
) -> AccountModification {
    AccountModification {
        pubkey: *account_pubkey,
        lamports: Some(account.lamports),
        owner: Some(account.owner),
        executable: Some(account.executable),
        data: Some(account.data.clone()),
        rent_epoch: Some(account.rent_epoch),
    }
}

async fn get_account(
    cluster: &Cluster,
    pubkey: &Pubkey,
) -> Result<Account, solana_rpc_client_api::client_error::Error> {
    // TODO(vbrunet)
    //  - Long term this should probably use the validator's AccountFetcher
    //  - Tracked here: https://github.com/magicblock-labs/magicblock-validator/issues/136
    Ok(RpcClient::new_with_commitment(
        cluster.url().to_string(),
        CommitmentConfig::confirmed(),
    )
    .get_account(&pubkey)
    .await?)
}
