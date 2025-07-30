use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use async_trait::async_trait;
use magicblock_committor_service::{
    tasks::tasks::CommitTask,
    transaction_preperator::{
        delivery_preparator::DeliveryPreparator,
        transaction_preparator::TransactionPreparatorV1,
    },
    types::{ScheduledL1MessageWrapper, TriggerType},
    ComputeBudgetConfig,
};
use magicblock_program::magic_scheduled_l1_message::{
    CommittedAccountV2, ScheduledL1Message,
};
use magicblock_rpc_client::MagicblockRpcClient;
use magicblock_table_mania::{GarbageCollectorConfig, TableMania};
use solana_account::Account;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::{CommitmentConfig, CommitmentLevel},
    signature::Keypair,
    signer::Signer,
    system_program,
};
use magicblock_committor_service::commit_scheduler::commit_id_tracker::{CommitIdFetcher, CommitIdTrackerResult};

// Helper function to create a test RPC client
pub async fn create_test_client() -> MagicblockRpcClient {
    let url = "http://localhost:9002".to_string();
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

    pub fn create_delivery_preparator(&self) -> DeliveryPreparator {
        DeliveryPreparator::new(
            self.rpc_client.clone(),
            self.table_mania.clone(),
            self.compute_budget_config.clone(),
        )
    }

    pub fn create_transaction_preparator(&self) -> TransactionPreparatorV1<MockCommitIdFetcher> {
        TransactionPreparatorV1::<MockCommitIdFetcher>::new(
            self.rpc_client.clone(),
            self.table_mania.clone(),
            self.compute_budget_config.clone(),
            Arc::new(MockCommitIdFetcher)
        )
    }
}

pub struct MockCommitIdFetcher;

#[async_trait::async_trait]
impl CommitIdFetcher for MockCommitIdFetcher {
    async fn fetch_commit_ids(&self, pubkeys: &[Pubkey]) -> CommitIdTrackerResult<HashMap<Pubkey, u64>> {
        Ok(pubkeys.iter().map(|pubkey| (*pubkey, 0)).collect())
    }

    fn peek_commit_id(&self, pubkey: &Pubkey) -> Option<u64> {
        None
    }
}


pub fn generate_random_bytes(length: usize) -> Vec<u8> {
    use rand::Rng;

    let mut rng = rand::thread_rng();
    (0..length).map(|_| rng.gen()).collect()
}

pub fn create_commit_task(data: &[u8]) -> CommitTask {
    static COMMIT_ID: AtomicU64 = AtomicU64::new(0);
    CommitTask {
        commit_id: COMMIT_ID.fetch_add(1, Ordering::Relaxed),
        allow_undelegation: false,
        committed_account: CommittedAccountV2 {
            pubkey: Pubkey::new_unique(),
            account: Account {
                lamports: 1000,
                data: data.to_vec(),
                owner: dlp::id(),
                executable: false,
                rent_epoch: 0,
            },
        },
    }
}

pub fn create_committed_account(data: &[u8]) -> CommittedAccountV2 {
    CommittedAccountV2 {
        pubkey: Pubkey::new_unique(),
        account: Account {
            lamports: 1000,
            data: data.to_vec(),
            owner: dlp::id(),
            executable: false,
            rent_epoch: 0,
        },
    }
}
