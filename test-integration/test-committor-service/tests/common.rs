use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use magicblock_committor_service::intent_executor::IntentExecutorImpl;
use magicblock_committor_service::{
    intent_executor::task_info_fetcher::{
        TaskInfoFetcher, TaskInfoFetcherResult,
    },
    tasks::tasks::CommitTask,
    transaction_preparator::{
        delivery_preparator::DeliveryPreparator,
        transaction_preparator::TransactionPreparatorV1,
    },
    ComputeBudgetConfig,
};
use magicblock_program::magic_scheduled_base_intent::CommittedAccountV2;
use magicblock_rpc_client::MagicblockRpcClient;
use magicblock_table_mania::{GarbageCollectorConfig, TableMania};
use solana_account::Account;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig, signature::Keypair, signer::Signer,
};

// Helper function to create a test RPC client
pub async fn create_test_client() -> MagicblockRpcClient {
    let url = "http://localhost:7799".to_string();
    let rpc_client =
        RpcClient::new_with_commitment(url, CommitmentConfig::confirmed());

    MagicblockRpcClient::new(Arc::new(rpc_client))
}

// Test fixture structure
pub struct TestFixture {
    pub rpc_client: MagicblockRpcClient,
    table_mania: TableMania,
    pub authority: Keypair,
    pub compute_budget_config: ComputeBudgetConfig,
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

    #[allow(dead_code)]
    pub fn create_delivery_preparator(&self) -> DeliveryPreparator {
        DeliveryPreparator::new(
            self.rpc_client.clone(),
            self.table_mania.clone(),
            self.compute_budget_config.clone(),
        )
    }

    #[allow(dead_code)]
    pub fn create_transaction_preparator(&self) -> TransactionPreparatorV1 {
        TransactionPreparatorV1::new(
            self.rpc_client.clone(),
            self.table_mania.clone(),
            self.compute_budget_config.clone(),
        )
    }

    #[allow(dead_code)]
    pub fn create_intent_executor(
        &self,
    ) -> IntentExecutorImpl<TransactionPreparatorV1, MockTaskInfoFetcher> {
        let transaction_preparator = self.create_transaction_preparator();
        let task_info_fetcher = Arc::new(MockTaskInfoFetcher);

        IntentExecutorImpl::new(
            self.rpc_client.clone(),
            transaction_preparator,
            task_info_fetcher,
        )
    }
}

pub struct MockTaskInfoFetcher;

#[async_trait::async_trait]
impl TaskInfoFetcher for MockTaskInfoFetcher {
    async fn fetch_next_commit_ids(
        &self,
        pubkeys: &[Pubkey],
    ) -> TaskInfoFetcherResult<HashMap<Pubkey, u64>> {
        Ok(pubkeys.iter().map(|pubkey| (*pubkey, 0)).collect())
    }

    async fn fetch_rent_reimbursements(
        &self,
        pubkeys: &[Pubkey],
    ) -> TaskInfoFetcherResult<Vec<Pubkey>> {
        Ok(pubkeys.to_vec())
    }

    fn peek_commit_id(&self, _pubkey: &Pubkey) -> Option<u64> {
        None
    }
}

#[allow(dead_code)]
pub fn generate_random_bytes(length: usize) -> Vec<u8> {
    use rand::Rng;

    let mut rng = rand::thread_rng();
    (0..length).map(|_| rng.gen()).collect()
}

#[allow(dead_code)]
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

#[allow(dead_code)]
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
