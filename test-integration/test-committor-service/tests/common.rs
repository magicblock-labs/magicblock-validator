use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};

use async_trait::async_trait;
use magicblock_committor_service::{
    intent_executor::{
        task_info_fetcher::{
            CacheTaskInfoFetcher, TaskInfoFetcher, TaskInfoFetcherError,
            TaskInfoFetcherResult,
        },
        IntentExecutorImpl,
    },
    tasks::commit_task::{CommitDelivery, CommitTask},
    transaction_preparator::{
        delivery_preparator::DeliveryPreparator, TransactionPreparatorImpl,
    },
    ComputeBudgetConfig, DEFAULT_ACTIONS_TIMEOUT,
};
use magicblock_core::{
    intent::{BaseActionCallback, CommittedAccount},
    traits::{ActionResult, ActionsCallbackScheduler, CallbackScheduleError},
};
use magicblock_rpc_client::MagicblockRpcClient;
use magicblock_table_mania::{GarbageCollectorConfig, TableMania};
use solana_account::Account;
use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    signature::{Keypair, Signature},
    signer::Signer,
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
    pub table_mania: TableMania,
    #[allow(dead_code)]
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
    ) -> IntentExecutorImpl<
        TransactionPreparatorImpl,
        MockTaskInfoFetcher,
        MockActionsCallbackExecutor,
    > {
        let transaction_preparator = self.create_transaction_preparator();

        IntentExecutorImpl::new(
            self.rpc_client.clone(),
            transaction_preparator,
            self.create_task_info_fetcher(),
            MockActionsCallbackExecutor::default(),
            DEFAULT_ACTIONS_TIMEOUT,
        )
    }

    #[allow(dead_code)]
    pub fn create_task_info_fetcher(
        &self,
    ) -> Arc<CacheTaskInfoFetcher<MockTaskInfoFetcher>> {
        Arc::new(CacheTaskInfoFetcher::new(MockTaskInfoFetcher(
            self.rpc_client.clone(),
        )))
    }
}

type CallbackCalls = Vec<(Vec<BaseActionCallback>, ActionResult)>;

#[derive(Clone, Default)]
pub struct MockActionsCallbackExecutor {
    pub calls: Arc<Mutex<CallbackCalls>>,
}

impl MockActionsCallbackExecutor {
    #[allow(dead_code)]
    pub fn calls(&self) -> CallbackCalls {
        self.calls.lock().unwrap().clone()
    }
}

impl ActionsCallbackScheduler for MockActionsCallbackExecutor {
    fn schedule(
        &self,
        callbacks: Vec<BaseActionCallback>,
        _signature: Option<Signature>,
        result: ActionResult,
    ) -> Vec<Result<Signature, CallbackScheduleError>> {
        let signatures = callbacks
            .iter()
            .map(|_| Ok(Signature::new_unique()))
            .collect();
        self.calls.lock().unwrap().push((callbacks, result));
        signatures
    }
}

pub struct MockTaskInfoFetcher(MagicblockRpcClient);

#[async_trait]
impl TaskInfoFetcher for MockTaskInfoFetcher {
    async fn fetch_next_commit_nonces(
        &self,
        pubkeys: &[Pubkey],
        _: u64,
    ) -> TaskInfoFetcherResult<HashMap<Pubkey, u64>> {
        Ok(pubkeys.iter().map(|pubkey| (*pubkey, 0)).collect())
    }

    async fn fetch_current_commit_nonces(
        &self,
        pubkeys: &[Pubkey],
        _: u64,
    ) -> TaskInfoFetcherResult<HashMap<Pubkey, u64>> {
        Ok(pubkeys.iter().map(|pubkey| (*pubkey, 0)).collect())
    }

    async fn fetch_rent_reimbursements(
        &self,
        pubkeys: &[Pubkey],
        _: u64,
    ) -> TaskInfoFetcherResult<Vec<Pubkey>> {
        Ok(pubkeys.to_vec())
    }

    async fn get_base_accounts(
        &self,
        pubkeys: &[Pubkey],
        _: u64,
    ) -> TaskInfoFetcherResult<HashMap<Pubkey, Account>> {
        self.0
            .get_multiple_accounts(pubkeys, None)
            .await
            .map_err(|err| {
                TaskInfoFetcherError::MagicBlockRpcClientError(Box::new(err))
            })
            .map(|accounts| {
                pubkeys
                    .iter()
                    .zip(accounts)
                    .filter_map(|(key, value)| value.map(|value| (*key, value)))
                    .collect()
            })
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
        committed_account: CommittedAccount {
            pubkey: Pubkey::new_unique(),
            account: Account {
                lamports: 1000,
                data: data.to_vec(),
                owner: dlp_api::id(),
                executable: false,
                rent_epoch: 0,
            },
            remote_slot: Default::default(),
        },
        delivery_details: CommitDelivery::StateInArgs,
    }
}

#[allow(dead_code)]
pub fn create_buffer_commit_task(data: &[u8]) -> CommitTask {
    let task = create_commit_task(data);
    let stage = task.state_preparation_stage();
    CommitTask {
        delivery_details: CommitDelivery::StateInBuffer { stage },
        ..task
    }
}

#[allow(dead_code)]
pub fn create_committed_account(data: &[u8]) -> CommittedAccount {
    CommittedAccount {
        pubkey: Pubkey::new_unique(),
        account: Account {
            lamports: 1000,
            data: data.to_vec(),
            owner: dlp_api::id(),
            executable: false,
            rent_epoch: 0,
        },
        remote_slot: Default::default(),
    }
}
