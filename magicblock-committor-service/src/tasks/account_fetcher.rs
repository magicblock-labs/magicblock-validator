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

impl Default for AccountFetcher {
    fn default() -> Self {
        Self::new()
    }
}

impl AccountFetcher {
    pub fn new() -> Self {
        use crate::{config::ChainConfig, ComputeBudgetConfig};

        #[cfg(feature = "dev-context-only-utils")]
        let chain_config =
            ChainConfig::local(ComputeBudgetConfig::new(1_000_000));

        #[cfg(not(feature = "dev-context-only-utils"))]
        let chain_config = ChainConfig {
            rpc_uri: magicblock_config::MagicBlockConfig::parse_config()
                .config
                .accounts
                .remote
                .url
                .as_ref()
                .map(|url| url.to_string())
                .unwrap_or_else(|| {
                    log::error!(
                        "Remote URL not configured, falling back to mainnet"
                    );
                    "https://api.mainnet-beta.solana.com".to_string()
                }),
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
