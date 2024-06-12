use std::str::FromStr;

use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    account::Account, bpf_loader_upgradeable, clock::Slot,
    commitment_config::CommitmentConfig, pubkey::Pubkey,
};

use crate::{
    chainparser,
    errors::{MutatorError, MutatorResult},
    program_account::adjust_deployment_slot,
    AccountModification, Cluster,
};

pub async fn mods_to_clone_account(
    cluster: &Cluster,
    account_address: &str,
    account: Option<Account>,
    slot: Slot,
    overrides: Option<AccountModification>,
) -> MutatorResult<Vec<AccountModification>> {
    // Fetch all accounts to clone

    // 1. Download the account info if needed
    let account_pubkey = Pubkey::from_str(account_address)?;
    let account = match account {
        Some(account) => account,
        None => {
            client_for_cluster(cluster)
                .get_account(&account_pubkey)
                .await?
        }
    };

    // 2. If the account is executable, find its executable address
    let executable_info = if account.executable {
        let executable_pubkey = get_executable_address(account_address)?;

        // 2.1. Download the executable account
        let mut executable_account = client_for_cluster(cluster)
            .get_account(&executable_pubkey)
            .await?;

        // 2.2. If we didn't find it then something is off and cloning the program
        //      account won't make sense either
        if executable_account.lamports == 0 {
            return Err(MutatorError::CouldNotFindExecutableDataAccount(
                executable_pubkey.to_string(),
                account_address.to_string(),
            ));
        }

        adjust_deployment_slot(
            &account_pubkey,
            &executable_pubkey,
            &account,
            Some(&mut executable_account),
            slot,
        )?;

        Some((executable_account, executable_pubkey))
    } else {
        None
    };

    // 3. If the account is executable, try to find its IDL account
    let idl_account_info = if account.executable {
        let (anchor_idl_address, shank_idl_address) =
            get_idl_addresses(account_address)?;

        // 3.1. Download the IDL account, try the anchor address first followed by shank
        if let Some(anchor_account_info) =
            maybe_get_idl_account(cluster, anchor_idl_address).await
        {
            Some(anchor_account_info)
        } else {
            maybe_get_idl_account(cluster, shank_idl_address).await
        }
    } else {
        None
    };
    let account_mod = {
        let mut account_mod =
            AccountModification::from((&account, account_address));
        if let Some(overrides) = overrides {
            account_mod.apply_overrides(&overrides);
        }
        account_mod
    };
    // 4. Convert to a vec of account modifications to apply
    Ok(vec![
        Some(account_mod),
        executable_info.map(|(account, address)| {
            AccountModification::from((&account, address.to_string().as_str()))
        }),
        idl_account_info.map(|(account, address)| {
            AccountModification::from((&account, address.to_string().as_str()))
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
    program_id: &str,
) -> Result<Pubkey, Box<dyn std::error::Error>> {
    let program_pubkey = Pubkey::from_str(program_id)?;
    let bpf_loader_id = bpf_loader_upgradeable::id();
    let seeds = &[program_pubkey.as_ref()];
    let (executable_address, _) =
        Pubkey::find_program_address(seeds, &bpf_loader_id);
    Ok(executable_address)
}

fn get_idl_addresses(
    program_id: &str,
) -> Result<(Option<Pubkey>, Option<Pubkey>), Box<dyn std::error::Error>> {
    let program_pubkey = Pubkey::from_str(program_id)?;
    Ok(chainparser::get_idl_addresses(&program_pubkey))
}
