use borsh::BorshDeserialize;
use magicblock_committor_program::Chunks;
use magicblock_committor_service::{
    persist::IntentPersisterImpl,
    tasks::{
        commit_stage_task::CleanupTask,
        task_strategist::{TaskStrategist, TransactionStrategy},
        utils::create_commit_finalize_task,
        BaseActionTask, BaseActionTaskV1, BaseTaskImpl, UndelegateTask,
    },
    transaction_preparator::TransactionPreparator,
    transactions::PreparedMessage,
};
use magicblock_core::intent::{BaseAction, ProgramArgs};
use magicblock_program::args::ShortAccountMeta;
use solana_pubkey::Pubkey;
use solana_sdk::signer::Signer;
use solana_sdk_ids::system_program;

use crate::common::{
    create_buffer_commit_finalize_task, create_committed_account,
    generate_random_bytes, TestFixture,
};

mod common;

#[tokio::test]
async fn test_prepare_commit_tx_with_single_account() {
    let fixture = TestFixture::new().await;
    let preparator = fixture.create_transaction_preparator();

    // Create test data
    let account_data = vec![1, 2, 3, 4, 5];
    let committed_account = create_committed_account(&account_data);

    let tasks: Vec<BaseTaskImpl> = vec![create_commit_finalize_task(
        1,
        true,
        committed_account.clone(),
        None,
    )
    .into()];
    let mut tx_strategy = TransactionStrategy {
        optimized_tasks: tasks,
        lookup_tables_keys: vec![],
        uniqueness_nonce: None,
    };

    // Test preparation
    let result = preparator
        .prepare_for_strategy(
            &fixture.authority,
            &mut tx_strategy,
            &None::<IntentPersisterImpl>,
        )
        .await;

    assert!(result.is_ok(), "Preparation failed: {:?}", result.err());

    assert!(matches!(result.unwrap(), PreparedMessage::V1(_)));
}

#[tokio::test]
async fn test_prepare_commit_tx_with_multiple_accounts() {
    let fixture = TestFixture::new().await;
    let preparator = fixture.create_transaction_preparator();

    let account1_data = generate_random_bytes(20);
    let committed_account1 = create_committed_account(&account1_data);

    let account2_data = generate_random_bytes(12);
    let committed_account2 = create_committed_account(&account2_data);

    let mut buffer_commit_task =
        create_buffer_commit_finalize_task(&account2_data);
    buffer_commit_task.committed_account.pubkey = committed_account2.pubkey;
    // Create test data
    let tasks: Vec<BaseTaskImpl> = vec![
        // account 1
        create_commit_finalize_task(1, true, committed_account1.clone(), None)
            .into(),
        // account 2
        buffer_commit_task.into(),
    ];
    let mut tx_strategy = TransactionStrategy {
        optimized_tasks: tasks,
        lookup_tables_keys: vec![],
        uniqueness_nonce: None,
    };

    // Test preparation
    preparator
        .prepare_for_strategy(
            &fixture.authority,
            &mut tx_strategy,
            &None::<IntentPersisterImpl>,
        )
        .await
        .unwrap();

    for task in &tx_strategy.optimized_tasks {
        let commit_task = match task {
            BaseTaskImpl::CommitFinalize(ct) => ct,
            _ => continue,
        };
        let Some(cleanup_task) = CleanupTask::from_commit_finalize(commit_task)
        else {
            continue;
        };
        let chunks_pda = cleanup_task.chunks_pda(&fixture.authority.pubkey());
        let chunks_account = fixture
            .rpc_client
            .get_account(&chunks_pda)
            .await
            .unwrap()
            .unwrap();
        let chunks = Chunks::try_from_slice(&chunks_account.data).unwrap();

        assert!(chunks.is_complete());
    }
}

#[tokio::test]
async fn test_prepare_commit_tx_with_base_actions() {
    let fixture = TestFixture::new().await;
    let preparator = fixture.create_transaction_preparator();

    // Create test data
    let committed_account = create_committed_account(&[1, 2, 3]);
    let base_action = BaseAction {
        id: 0,
        compute_units: 30_000,
        destination_program: system_program::id(),
        source_program: None,
        escrow_authority: fixture.authority.pubkey(),
        data_per_program: ProgramArgs {
            escrow_index: 0,
            data: vec![4, 5, 6],
        },
        account_metas_per_program: vec![ShortAccountMeta {
            pubkey: Pubkey::new_unique(),
            is_writable: true,
        }],
        callback: None,
    };

    let mut buffer_commit_task =
        create_buffer_commit_finalize_task(&committed_account.account.data);
    buffer_commit_task.committed_account.pubkey = committed_account.pubkey;
    let tasks: Vec<BaseTaskImpl> = vec![
        // commit account
        buffer_commit_task.into(),
        // BaseAction
        BaseActionTask::V1(BaseActionTaskV1 {
            action: base_action,
        })
        .into(),
    ];

    // Test preparation
    let mut tx_strategy = TransactionStrategy {
        optimized_tasks: tasks,
        lookup_tables_keys: vec![],
        uniqueness_nonce: None,
    };

    // Test preparation
    preparator
        .prepare_for_strategy(
            &fixture.authority,
            &mut tx_strategy,
            &None::<IntentPersisterImpl>,
        )
        .await
        .unwrap();

    // Now we verify that buffers were created
    for task in &tx_strategy.optimized_tasks {
        let commit_task = match task {
            BaseTaskImpl::CommitFinalize(ct) => ct,
            _ => continue,
        };
        let Some(cleanup_task) = CleanupTask::from_commit_finalize(commit_task)
        else {
            continue;
        };
        let chunks_pda = cleanup_task.chunks_pda(&fixture.authority.pubkey());

        let chunks_account = fixture
            .rpc_client
            .get_account(&chunks_pda)
            .await
            .unwrap()
            .unwrap();
        let chunks = Chunks::try_from_slice(&chunks_account.data).unwrap();

        assert!(chunks.is_complete());
    }
}

#[tokio::test]
async fn test_prepare_undelegate_tx_with_alts() {
    let fixture = TestFixture::new().await;
    let preparator = fixture.create_transaction_preparator();

    // Create test data
    let committed_account = create_committed_account(&[1, 2, 3]);
    let tasks: Vec<BaseTaskImpl> = vec![
        // Undelegate
        UndelegateTask {
            delegated_account: committed_account.pubkey,
            owner_program: Pubkey::new_unique(),
            rent_reimbursement: Pubkey::new_unique(),
            include_undelegation_request: false,
        }
        .into(),
    ];

    let lookup_tables_keys = TaskStrategist::collect_lookup_table_keys(
        &fixture.authority.pubkey(),
        &tasks,
        None,
    );
    let mut tx_strategy = TransactionStrategy {
        optimized_tasks: tasks,
        lookup_tables_keys,
        uniqueness_nonce: None,
    };

    // Test preparation
    let result = preparator
        .prepare_for_strategy(
            &fixture.authority,
            &mut tx_strategy,
            &None::<IntentPersisterImpl>,
        )
        .await;

    assert!(result.is_ok());
}
