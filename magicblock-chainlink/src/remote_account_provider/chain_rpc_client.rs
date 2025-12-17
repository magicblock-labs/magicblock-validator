use std::sync::Arc;

use async_trait::async_trait;
use solana_account::Account;
use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_rpc_client_api::{
    client_error::Result as ClientResult, config::RpcAccountInfoConfig,
    response::RpcResult,
};

// -----------------
// Trait
// -----------------
#[async_trait]
pub trait ChainRpcClient: Send + Sync + Clone + 'static {
    fn commitment(&self) -> CommitmentConfig;
    async fn get_account_with_config(
        &self,
        pubkey: &Pubkey,
        config: RpcAccountInfoConfig,
    ) -> RpcResult<Option<Account>>;

    async fn get_multiple_accounts_with_config(
        &self,
        pubkeys: &[Pubkey],
        config: RpcAccountInfoConfig,
    ) -> RpcResult<Vec<Option<Account>>>;

    async fn get_slot_with_commitment(
        &self,
        commitment: CommitmentConfig,
    ) -> ClientResult<u64>;
}

// -----------------
// Implementation
// -----------------
#[derive(Clone)]
pub struct ChainRpcClientImpl {
    pub(crate) rpc_client: Arc<RpcClient>,
}

impl ChainRpcClientImpl {
    pub fn new(rpc_client: RpcClient) -> Self {
        Self {
            rpc_client: Arc::new(rpc_client),
        }
    }

    pub fn new_from_url(rpc_url: &str, commitment: CommitmentConfig) -> Self {
        let client =
            RpcClient::new_with_commitment(rpc_url.to_string(), commitment);
        Self::new(client)
    }
}

#[async_trait]
impl ChainRpcClient for ChainRpcClientImpl {
    fn commitment(&self) -> CommitmentConfig {
        self.rpc_client.commitment()
    }

    async fn get_account_with_config(
        &self,
        pubkey: &Pubkey,
        config: RpcAccountInfoConfig,
    ) -> RpcResult<Option<Account>> {
        self.rpc_client
            .get_account_with_config(pubkey, config)
            .await
    }
    async fn get_multiple_accounts_with_config(
        &self,
        pubkeys: &[Pubkey],
        config: RpcAccountInfoConfig,
    ) -> RpcResult<Vec<Option<Account>>> {
        self.rpc_client
            .get_multiple_accounts_with_config(pubkeys, config)
            .await
    }
    async fn get_slot_with_commitment(
        &self,
        commitment: CommitmentConfig,
    ) -> ClientResult<u64> {
        self.rpc_client.get_slot_with_commitment(commitment).await
    }
}
