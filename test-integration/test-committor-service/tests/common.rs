use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use async_trait::async_trait;
use light_client::indexer::photon_indexer::PhotonIndexer;
use light_compressed_account::instruction_data::compressed_proof::ValidityProof;
use light_sdk_types::instruction::account_meta::CompressedAccountMeta;
use magicblock_chainlink::testing::utils::PHOTON_URL;
use magicblock_committor_service::{
    intent_executor::{
        task_info_fetcher::{
            ResetType, TaskInfoFetcher, TaskInfoFetcherResult,
        },
        IntentExecutorImpl,
    },
    tasks::{task_builder::CompressedData, CommitTask, CompressedCommitTask},
    transaction_preparator::{
        delivery_preparator::DeliveryPreparator, TransactionPreparatorImpl,
    },
    ComputeBudgetConfig,
};
use magicblock_program::magic_scheduled_base_intent::CommittedAccount;
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

// Helper function to create a test PhotonIndexer
pub fn create_test_photon_indexer() -> Arc<PhotonIndexer> {
    let url = PHOTON_URL.to_string();
    Arc::new(PhotonIndexer::new(url, None))
}

// Test fixture structure
pub struct TestFixture {
    pub rpc_client: MagicblockRpcClient,
    #[allow(dead_code)]
    pub photon_client: Arc<PhotonIndexer>,
    pub table_mania: TableMania,
    pub authority: Keypair,
    pub compute_budget_config: ComputeBudgetConfig,
}

impl TestFixture {
    #[allow(dead_code)]
    pub async fn new() -> Self {
        let authority = Keypair::new();
        TestFixture::new_with_keypair(authority).await
    }

    pub async fn new_with_keypair(authority: Keypair) -> Self {
        let rpc_client = create_test_client().await;

        // PhotonIndexer
        let photon_client = create_test_photon_indexer();

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
            photon_client,
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
    pub fn create_transaction_preparator(&self) -> TransactionPreparatorImpl {
        TransactionPreparatorImpl::new(
            self.rpc_client.clone(),
            self.table_mania.clone(),
            self.compute_budget_config.clone(),
        )
    }

    #[allow(dead_code)]
    pub fn create_intent_executor(
        &self,
    ) -> IntentExecutorImpl<TransactionPreparatorImpl, MockTaskInfoFetcher>
    {
        let transaction_preparator = self.create_transaction_preparator();
        let task_info_fetcher = Arc::new(MockTaskInfoFetcher);

        IntentExecutorImpl::new(
            self.rpc_client.clone(),
            self.photon_client.clone(),
            transaction_preparator,
            task_info_fetcher,
        )
    }
}

pub struct MockTaskInfoFetcher;
#[async_trait]
impl TaskInfoFetcher for MockTaskInfoFetcher {
    async fn fetch_next_commit_ids(
        &self,
        pubkeys: &[Pubkey],
        _compressed: bool,
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

    fn reset(&self, _: ResetType) {}
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
        committed_account: CommittedAccount {
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
pub fn create_compressed_commit_task(
    pubkey: Pubkey,
    hash: [u8; 32],
    data: &[u8],
) -> CompressedCommitTask {
    static COMMIT_ID: AtomicU64 = AtomicU64::new(0);
    CompressedCommitTask {
        commit_id: COMMIT_ID.fetch_add(1, Ordering::Relaxed),
        compressed_data: CompressedData {
            hash,
            compressed_delegation_record_bytes: vec![],
            remaining_accounts: vec![],
            account_meta: CompressedAccountMeta::default(),
            proof: ValidityProof::default(),
        },
        allow_undelegation: false,
        committed_account: CommittedAccount {
            pubkey,
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
pub fn create_committed_account(data: &[u8]) -> CommittedAccount {
    CommittedAccount {
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
