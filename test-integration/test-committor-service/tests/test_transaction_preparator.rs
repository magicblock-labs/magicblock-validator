use crate::common::{
    create_committed_account, generate_random_bytes, TestFixture,
};
use borsh::BorshDeserialize;
use dlp::args::Context;
use magicblock_committor_program::Chunks;
use magicblock_committor_service::tasks::task_strategist::{
    TaskStrategist, TransactionStrategy,
};
use magicblock_committor_service::tasks::tasks::{
    ArgsTask, BaseTask, BufferTask, CommitTask, FinalizeTask, L1ActionTask,
    UndelegateTask,
};
use magicblock_committor_service::tasks::utils::TransactionUtils;
use magicblock_committor_service::{
    persist::IntentPersisterImpl,
    transaction_preparator::transaction_preparator::TransactionPreparator,
};
use magicblock_program::magic_scheduled_base_intent::{
    BaseAction, ProgramArgs, ShortAccountMeta,
};
use solana_pubkey::Pubkey;
use solana_sdk::{signer::Signer, system_program};

mod common;

#[tokio::test]
async fn test_prepare_commit_tx_with_single_account() {
    let fixture = TestFixture::new().await;
    let preparator = fixture.create_transaction_preparator();

    // Create test data
    let account_data = vec![1, 2, 3, 4, 5];
    let committed_account = create_committed_account(&account_data);

    let tasks = vec![
        Box::new(ArgsTask::Commit(CommitTask {
            commit_id: 1,
            committed_account: committed_account.clone(),
            allow_undelegation: true,
        })) as Box<dyn BaseTask>,
        Box::new(ArgsTask::Finalize(FinalizeTask {
            delegated_account: committed_account.pubkey,
        })),
    ];
    let tx_strategy = TransactionStrategy {
        optimized_tasks: tasks,
        lookup_tables_keys: vec![],
    };

    // Test preparation
    let result = preparator
        .prepare_for_strategy(
            &fixture.authority,
            &tx_strategy,
            &None::<IntentPersisterImpl>,
        )
        .await;

    assert!(result.is_ok(), "Preparation failed: {:?}", result.err());

    // For such strategy there's no preparation
    // expected messsage is just assembled tx from Args task with no ALTs
    let mut actual_message = result.unwrap();
    let expected_message = TransactionUtils::assemble_tasks_tx(
        &fixture.authority,
        &tx_strategy.optimized_tasks,
        fixture.compute_budget_config.compute_unit_price,
        &[],
    )
    .unwrap()
    .message;

    // Block hash is random in result of prepare_for_strategy
    // should be set be caller, so here we just set value of expected for test
    actual_message.set_recent_blockhash(*expected_message.recent_blockhash());
    assert_eq!(actual_message, expected_message)
}

#[tokio::test]
async fn test_prepare_commit_tx_with_multiple_accounts() {
    let fixture = TestFixture::new().await;
    let preparator = fixture.create_transaction_preparator();

    let account1_data = generate_random_bytes(20);
    let committed_account1 = create_committed_account(&account1_data);

    let account2_data = generate_random_bytes(12);
    let committed_account2 = create_committed_account(&account2_data);

    let buffer_commit_task = BufferTask::Commit(CommitTask {
        commit_id: 1,
        committed_account: committed_account2.clone(),
        allow_undelegation: true,
    });
    // Create test data
    let tasks = vec![
        // account 1
        Box::new(ArgsTask::Commit(CommitTask {
            commit_id: 1,
            committed_account: committed_account1.clone(),
            allow_undelegation: true,
        })) as Box<dyn BaseTask>,
        // account 2
        Box::new(buffer_commit_task.clone()),
        // finalize account 1
        Box::new(ArgsTask::Finalize(FinalizeTask {
            delegated_account: committed_account1.pubkey,
        })),
        // finalize account 2
        Box::new(ArgsTask::Finalize(FinalizeTask {
            delegated_account: committed_account2.pubkey,
        })),
    ];
    let tx_strategy = TransactionStrategy {
        optimized_tasks: tasks,
        lookup_tables_keys: vec![],
    };

    // Test preparation
    let mut actual_message = preparator
        .prepare_for_strategy(
            &fixture.authority,
            &tx_strategy,
            &None::<IntentPersisterImpl>,
        )
        .await
        .unwrap();

    let expected_message = TransactionUtils::assemble_tasks_tx(
        &fixture.authority,
        &tx_strategy.optimized_tasks,
        fixture.compute_budget_config.compute_unit_price,
        &[],
    )
    .unwrap()
    .message;

    // Block hash is random in result of prepare_for_strategy
    // should be set be caller, so here we just set value of expected for test
    actual_message.set_recent_blockhash(*expected_message.recent_blockhash());
    assert_eq!(actual_message, expected_message);

    // Now we verify that buffers were created
    let preparation_info = buffer_commit_task
        .preparation_info(&fixture.authority.pubkey())
        .unwrap();

    let chunks_account = fixture
        .rpc_client
        .get_account(&preparation_info.chunks_pda)
        .await
        .unwrap()
        .unwrap();
    let chunks = Chunks::try_from_slice(&chunks_account.data).unwrap();

    assert!(chunks.is_complete());
}

#[tokio::test]
async fn test_prepare_commit_tx_with_l1_actions() {
    let fixture = TestFixture::new().await;
    let preparator = fixture.create_transaction_preparator();

    // Create test data
    let committed_account = create_committed_account(&[1, 2, 3]);
    let base_action = BaseAction {
        compute_units: 30_000,
        destination_program: system_program::id(),
        escrow_authority: fixture.authority.pubkey(),
        data_per_program: ProgramArgs {
            escrow_index: 0,
            data: vec![4, 5, 6],
        },
        account_metas_per_program: vec![ShortAccountMeta {
            pubkey: Pubkey::new_unique(),
            is_writable: true,
        }],
    };

    let buffer_commit_task = BufferTask::Commit(CommitTask {
        commit_id: 1,
        committed_account: committed_account.clone(),
        allow_undelegation: true,
    });
    let tasks = vec![
        // commit account
        Box::new(buffer_commit_task.clone()) as Box<dyn BaseTask>,
        // finalize account
        Box::new(ArgsTask::Finalize(FinalizeTask {
            delegated_account: committed_account.pubkey,
        })),
        // L1Action
        Box::new(ArgsTask::L1Action(L1ActionTask {
            context: Context::Commit,
            action: base_action,
        })),
    ];

    // Test preparation
    let tx_strategy = TransactionStrategy {
        optimized_tasks: tasks,
        lookup_tables_keys: vec![],
    };

    // Test preparation
    let mut actual_message = preparator
        .prepare_for_strategy(
            &fixture.authority,
            &tx_strategy,
            &None::<IntentPersisterImpl>,
        )
        .await
        .unwrap();

    let expected_message = TransactionUtils::assemble_tasks_tx(
        &fixture.authority,
        &tx_strategy.optimized_tasks,
        fixture.compute_budget_config.compute_unit_price,
        &[],
    )
    .unwrap()
    .message;

    // Block hash is random in result of prepare_for_strategy
    // should be set be caller, so here we just set value of expected for test
    actual_message.set_recent_blockhash(*expected_message.recent_blockhash());
    assert_eq!(actual_message, expected_message);

    // Now we verify that buffers were created
    let preparation_info = buffer_commit_task
        .preparation_info(&fixture.authority.pubkey())
        .unwrap();

    let chunks_account = fixture
        .rpc_client
        .get_account(&preparation_info.chunks_pda)
        .await
        .unwrap()
        .unwrap();
    let chunks = Chunks::try_from_slice(&chunks_account.data).unwrap();

    assert!(chunks.is_complete());
}

#[tokio::test]
async fn test_prepare_finalize_tx_with_undelegate_with_atls() {
    let fixture = TestFixture::new().await;
    let preparator = fixture.create_transaction_preparator();

    // Create test data
    let committed_account = create_committed_account(&[1, 2, 3]);
    let tasks: Vec<Box<dyn BaseTask>> = vec![
        // finalize account
        Box::new(ArgsTask::Finalize(FinalizeTask {
            delegated_account: committed_account.pubkey,
        })),
        // L1Action
        Box::new(ArgsTask::Undelegate(UndelegateTask {
            delegated_account: committed_account.pubkey,
            owner_program: Pubkey::new_unique(),
            rent_reimbursement: Pubkey::new_unique(),
        })),
    ];

    let lookup_tables_keys = TaskStrategist::collect_lookup_table_keys(
        &fixture.authority.pubkey(),
        &tasks,
    );
    let tx_strategy = TransactionStrategy {
        optimized_tasks: tasks,
        lookup_tables_keys,
    };

    // Test preparation
    let result = preparator
        .prepare_for_strategy(
            &fixture.authority,
            &tx_strategy,
            &None::<IntentPersisterImpl>,
        )
        .await;

    assert!(result.is_ok());
}
