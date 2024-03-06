use std::{str::FromStr, sync::Arc};

use crate::{chainparser, AccountModification};
use sleipnir_rpc_client::rpc_client::RpcClient;
use solana_sdk::{
    account::Account, bpf_loader_upgradeable, commitment_config::CommitmentConfig,
    genesis_config::ClusterType, pubkey::Pubkey,
};

use crate::errors::{MutatorError, MutatorResult};

const TESTNET_URL: &str = "https://api.testnet.solana.com";
const MAINNET_URL: &str = "https://api.mainnet-beta.solana.com";
const DEVNET_URL: &str = "https://api.devnet.solana.com";

#[derive(Clone)]
pub struct AccountProcessor {
    client_testnet: Arc<RpcClient>,
    client_mainnet: Arc<RpcClient>,
    client_devnet: Arc<RpcClient>,
}

impl AccountProcessor {
    pub fn new() -> Self {
        let client_testnet =
            RpcClient::new_with_commitment(TESTNET_URL.to_string(), CommitmentConfig::confirmed());
        let client_mainnet =
            RpcClient::new_with_commitment(MAINNET_URL.to_string(), CommitmentConfig::confirmed());
        let client_devnet =
            RpcClient::new_with_commitment(DEVNET_URL.to_string(), CommitmentConfig::confirmed());
        Self {
            client_testnet: Arc::new(client_testnet),
            client_mainnet: Arc::new(client_mainnet),
            client_devnet: Arc::new(client_devnet),
        }
    }

    pub async fn mods_to_clone_account(
        &self,
        cluster: ClusterType,
        account_address: &str,
    ) -> MutatorResult<Vec<AccountModification>> {
        // Fetch all accounts to clone

        // 1. Download the account info
        let account_pubkey = Pubkey::from_str(account_address)?;
        let account = self
            .client_for_cluster(cluster)
            .get_account(&account_pubkey)
            .await?;
        //
        // 2. If the account is executable, find its executable address
        let executable_info = if account.executable {
            let executable_address = get_executable_address(account_address)?;

            // 2.1. Download the executable account
            let executable_account = self
                .client_for_cluster(cluster)
                .get_account(&executable_address)
                .await?;

            // 2.2. If we didn't find it then something is off and cloning the program
            //      account won't make sense either
            if executable_account.lamports == 0 {
                return Err(MutatorError::CouldNotFindExecutableDataAccount(
                    executable_address.to_string(),
                    account_address.to_string(),
                ));
            }

            Some((executable_account, executable_address))
        } else {
            None
        };

        // 3. If the account is executable, try to find its IDL account
        let idl_account_info = if account.executable {
            let (anchor_idl_address, shank_idl_address) = get_idl_addresses(account_address)?;

            // 3.1. Download the IDL account, try the anchor address first followed by shank
            if let Some(anchor_account_info) = self
                .maybe_get_idl_account(cluster, anchor_idl_address)
                .await
            {
                Some(anchor_account_info)
            } else {
                self.maybe_get_idl_account(cluster, shank_idl_address).await
            }
        } else {
            None
        };

        // 4. Convert to a vec of account modifications to apply
        Ok(vec![
            Some(AccountModification::from((&account, account_address))),
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

    pub fn client_for_cluster(&self, cluster: ClusterType) -> Arc<RpcClient> {
        use ClusterType::*;
        match cluster {
            Testnet => self.client_testnet.clone(),
            MainnetBeta => self.client_mainnet.clone(),
            Devnet => self.client_devnet.clone(),
            Development => panic!("Development cluster not supported when cloning accounts"),
        }
    }

    async fn maybe_get_idl_account(
        &self,
        cluster: ClusterType,
        idl_address: Option<Pubkey>,
    ) -> Option<(Account, Pubkey)> {
        if let Some(idl_address) = idl_address {
            self.client_for_cluster(cluster)
                .get_account(&idl_address)
                .await
                .ok()
                .map(|account| (account, idl_address))
        } else {
            None
        }
    }
}

impl Default for AccountProcessor {
    fn default() -> Self {
        Self::new()
    }
}

fn get_executable_address(program_id: &str) -> Result<Pubkey, Box<dyn std::error::Error>> {
    let program_pubkey = Pubkey::from_str(program_id)?;
    let bpf_loader_id = bpf_loader_upgradeable::id();
    let seeds = &[program_pubkey.as_ref()];
    let (executable_address, _) = Pubkey::find_program_address(seeds, &bpf_loader_id);
    Ok(executable_address)
}

fn get_idl_addresses(
    program_id: &str,
) -> Result<(Option<Pubkey>, Option<Pubkey>), Box<dyn std::error::Error>> {
    let program_pubkey = Pubkey::from_str(program_id)?;
    Ok(chainparser::get_idl_addresses(&program_pubkey))
}
