use sleipnir_program::sleipnir_instruction::AccountModification;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    account::Account, bpf_loader_upgradeable, clock::Slot,
    commitment_config::CommitmentConfig, pubkey::Pubkey, signature::Keypair,
    signer::Signer,
};

use crate::{
    chainparser,
    errors::{MutatorError, MutatorResult},
    program_account::adjust_deployment_slot,
    Cluster,
};

fn account_modification_from_account(
    account_publey: &Pubkey,
    account: &Account,
) -> AccountModification {
    AccountModification {
        pubkey: *account_publey,
        lamports: Some(account.lamports),
        owner: Some(account.owner),
        executable: Some(account.executable),
        data: Some(account.data.clone()),
        rent_epoch: Some(account.rent_epoch),
    }
}

pub async fn mods_to_clone_account(
    cluster: &Cluster,
    account_pubkey: &Pubkey,
    account: Option<Account>,
    slot: Slot,
    overrides: Option<AccountModification>,
) -> MutatorResult<Vec<AccountModification>> {
    // Fetch all accounts to clone

    // 1. Download the account info if needed
    let account = match account {
        Some(account) => account,
        None => {
            client_for_cluster(cluster)
                .get_account(account_pubkey)
                .await?
        }
    };

    // 2. If the account is executable, find its executable address
    let executable_info = if account.executable {
        let executable_pubkey = get_executable_address(account_pubkey)?;

        // 2.1. Download the executable account
        let mut executable_account = client_for_cluster(cluster)
            .get_account(&executable_pubkey)
            .await
            .map_err(|err| {
                MutatorError::FailedToCloneProgramExecutableDataAccount(
                    *account_pubkey,
                    err,
                )
            })?;

        // 2.2. If we didn't find it then something is off and cloning the program
        //      account won't make sense either
        if executable_account.lamports == 0 {
            return Err(MutatorError::CouldNotFindExecutableDataAccount(
                executable_pubkey,
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
            account_pubkey,
            &executable_pubkey,
            &account,
            Some(&mut executable_account),
            targeted_deployment_slot,
        )?;

        let buffer_pubkey = Keypair::new().pubkey();

        Some((executable_account, executable_pubkey))
    } else {
        None
    };

    // 3. Apply any override needed
    let account_mod = {
        let mut account_mod =
            account_modification_from_account(account_pubkey, &account);
        if let Some(overrides) = overrides {
            if let Some(lamports) = overrides.lamports {
                account_mod.lamports = Some(lamports);
            }
            if let Some(owner) = &overrides.owner {
                account_mod.owner = Some(owner.clone());
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
    };

    // 4. Convert to a vec of account modifications to apply
    Ok(vec![
        Some(account_mod),
        executable_info.map(|(account, address)| {
            account_modification_from_account(&address, &account)
        }),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<AccountModification>>())
}

fn client_for_cluster(cluster: &Cluster) -> RpcClient {
    RpcClient::new_with_commitment(
        cluster.url().to_string(),
        CommitmentConfig::confirmed(),
    )
}

async fn maybe_get_idl_account(
    cluster: &Cluster,
    idl_address: Option<Pubkey>,
) -> Option<(Account, Pubkey)> {
    if let Some(idl_address) = idl_address {
        client_for_cluster(cluster)
            .get_account(&idl_address)
            .await
            .ok()
            .map(|account| (account, idl_address))
    } else {
        None
    }
}

pub(crate) fn get_executable_address(
    program_pubkey: &Pubkey,
) -> Result<Pubkey, Box<dyn std::error::Error>> {
    let bpf_loader_id = bpf_loader_upgradeable::id();
    let seeds = &[program_pubkey.as_ref()];
    let (executable_address, _) =
        Pubkey::find_program_address(seeds, &bpf_loader_id);
    Ok(executable_address)
}

fn get_idl_addresses(
    program_pubkey: &Pubkey,
) -> Result<(Option<Pubkey>, Option<Pubkey>), Box<dyn std::error::Error>> {
    Ok(chainparser::get_idl_addresses(&program_pubkey))
}
