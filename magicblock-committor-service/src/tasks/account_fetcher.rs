use magicblock_config::MagicBlockConfig;

use solana_account::Account;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::commitment_config::CommitmentConfig;

//
// AccountFetcher is used by CommitTask
//
pub struct AccountFetcher {
    rpc_client: RpcClient,
}

impl AccountFetcher {
    pub fn new() -> Self {
        use crate::{config::ChainConfig, ComputeBudgetConfig};
        let mb_config = MagicBlockConfig::parse_config();

        let chain_config = ChainConfig {
            rpc_uri: mb_config
                .config
                .accounts
                .remote
                .url
                .as_ref()
                .unwrap()
                .to_string(),
            ..ChainConfig::mainnet(ComputeBudgetConfig::new(1_000_000))
        };

        Self {
            rpc_client: RpcClient::new_with_commitment(
                chain_config.rpc_uri.to_string(),
                CommitmentConfig {
                    commitment: chain_config.commitment,
                },
            ),
        }
    }

    pub async fn fetch_account(
        &self,
        pubkey: &Pubkey,
    ) -> Result<Account, solana_rpc_client_api::client_error::Error> {
        self.rpc_client.get_account(pubkey).await
    }
}
