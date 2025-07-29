use std::sync::Arc;

use magicblock_committor_service::{
    transaction_preperator::delivery_preparator::DeliveryPreparator,
    ComputeBudgetConfig,
};
use magicblock_rpc_client::MagicblockRpcClient;
use magicblock_table_mania::{GarbageCollectorConfig, TableMania};
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::{CommitmentConfig, CommitmentLevel},
    signature::Keypair,
    signer::Signer,
    system_program,
};
use tempfile::TempDir;

// Helper function to create a test RPC client
pub async fn create_test_client() -> MagicblockRpcClient {
    let url = "http://localhost:8899".to_string();
    let rpc_client =
        RpcClient::new_with_commitment(url, CommitmentConfig::confirmed());

    MagicblockRpcClient::new(Arc::new(rpc_client))
}

// Test fixture structure
pub struct TestFixture {
    pub rpc_client: MagicblockRpcClient,
    table_mania: TableMania,
    pub authority: Keypair,
    compute_budget_config: ComputeBudgetConfig,
}

impl TestFixture {
    pub async fn new() -> Self {
        let authority = Keypair::new();
        let rpc_client = create_test_client().await;

        // TableMania
        let gc_config = GarbageCollectorConfig::default();
        let table_mania =
            TableMania::new(rpc_client.clone(), &authority, Some(gc_config));

        // Airdrop some SOL to the authority for testing
        rpc_client
            .request_airdrop(
                &authority.pubkey(),
                100_000_000_000, // 100 SOL
            )
            .await
            .unwrap();

        let compute_budget_config = ComputeBudgetConfig::new(1_000_000);
        Self {
            rpc_client,
            table_mania,
            authority,
            compute_budget_config,
        }
    }

    pub fn create_preparator(&self) -> DeliveryPreparator {
        DeliveryPreparator::new(
            self.rpc_client.clone(),
            self.table_mania.clone(),
            self.compute_budget_config.clone(),
        )
    }
}
