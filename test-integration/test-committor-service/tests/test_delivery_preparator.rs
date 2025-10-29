use borsh::BorshDeserialize;
use magicblock_committor_program::Chunks;
use magicblock_committor_service::{
    persist::IntentPersisterImpl,
    tasks::{
        args_task::{ArgsTask, ArgsTaskType},
        buffer_task::{BufferTask, BufferTaskType},
        task_strategist::{TaskStrategist, TransactionStrategy},
        BaseTask, PreparationState,
    },
};
use solana_sdk::signer::Signer;

use crate::common::{create_commit_task, generate_random_bytes, TestFixture};

mod common;

#[tokio::test]
async fn test_prepare_10kb_buffer() {
    let fixture = TestFixture::new().await;
    let preparator = fixture.create_delivery_preparator();

    let data = generate_random_bytes(10 * 1024);
    let buffer_task = BufferTaskType::Commit(create_commit_task(&data));
    let mut strategy = TransactionStrategy {
        optimized_tasks: vec![Box::new(BufferTask::new_preparation_required(
            buffer_task,
        ))],
        lookup_tables_keys: vec![],
    };

    // Test preparation
    let result = preparator
        .prepare_for_delivery(
            &fixture.authority,
            &mut strategy,
            &None::<IntentPersisterImpl>,
        )
        .await;

    assert!(result.is_ok(), "Preparation failed: {:?}", result.err());

    // Verify the buffer account was created and initialized
    let PreparationState::Cleanup(cleanup_task) =
        strategy.optimized_tasks[0].preparation_state()
    else {
        panic!("unexpected PreparationState");
    };

    let buffer_pda = cleanup_task.buffer_pda(&fixture.authority.pubkey());
    // Check buffer account exists
    let buffer_account = fixture
        .rpc_client
        .get_account(&buffer_pda)
        .await
        .unwrap()
        .expect("Buffer account should exist");

    assert_eq!(buffer_account.data, data, "Buffer account size mismatch");

    // Check chunks account exists
    let chunks_pda = cleanup_task.chunks_pda(&fixture.authority.pubkey());
    let chunks_account = fixture
        .rpc_client
        .get_account(&chunks_pda)
        .await
        .unwrap()
        .expect("Chunks account should exist");

    let chunks = Chunks::try_from_slice(&chunks_account.data)
        .expect("Failed to deserialize chunks");

    assert!(
        chunks.is_complete(),
        "Chunks should be marked as complete after preparation"
    );
}

#[tokio::test]
async fn test_prepare_multiple_buffers() {
    let fixture = TestFixture::new().await;
    let preparator = fixture.create_delivery_preparator();

    let datas = [
        generate_random_bytes(10 * 1024),
        generate_random_bytes(10),
        generate_random_bytes(500 * 1024),
    ];
    let buffer_tasks = datas
        .iter()
        .map(|data| {
            let task =
                BufferTaskType::Commit(create_commit_task(data.as_slice()));
            Box::new(BufferTask::new_preparation_required(task))
                as Box<dyn BaseTask>
        })
        .collect();
    let mut strategy = TransactionStrategy {
        optimized_tasks: buffer_tasks,
        lookup_tables_keys: vec![],
    };

    // Test preparation
    let result = preparator
        .prepare_for_delivery(
            &fixture.authority,
            &mut strategy,
            &None::<IntentPersisterImpl>,
        )
        .await;

    assert!(result.is_ok(), "Preparation failed: {:?}", result.err());

    // Verify the buffer account was created and initialized
    let cleanup_tasks = strategy.optimized_tasks.iter().map(|el| {
        let PreparationState::Cleanup(cleanup_task) = el.preparation_state()
        else {
            panic!("Unexpected preparation state!");
        };

        cleanup_task
    });

    for (i, cleanup_task) in cleanup_tasks.enumerate() {
        // Check buffer account exists
        let buffer_pda = cleanup_task.buffer_pda(&fixture.authority.pubkey());
        let buffer_account = fixture
            .rpc_client
            .get_account(&buffer_pda)
            .await
            .unwrap()
            .expect("Buffer account should exist");

        assert_eq!(
            buffer_account.data, datas[i],
            "Buffer account size mismatch"
        );

        // Check chunks account exists
        let chunks_pda = cleanup_task.chunks_pda(&fixture.authority.pubkey());
        let chunks_account = fixture
            .rpc_client
            .get_account(&chunks_pda)
            .await
            .unwrap()
            .expect("Chunks account should exist");

        let chunks = Chunks::try_from_slice(&chunks_account.data)
            .expect("Failed to deserialize chunks");

        assert!(
            chunks.is_complete(),
            "Chunks should be marked as complete after preparation"
        );
    }
}

#[tokio::test]
async fn test_lookup_tables() {
    let fixture = TestFixture::new().await;
    let preparator = fixture.create_delivery_preparator();

    let datas = [
        generate_random_bytes(10),
        generate_random_bytes(20),
        generate_random_bytes(30),
    ];
    let tasks = datas
        .iter()
        .map(|data| {
            let task =
                ArgsTaskType::Commit(create_commit_task(data.as_slice()));
            Box::<ArgsTask>::new(task.into()) as Box<dyn BaseTask>
        })
        .collect::<Vec<_>>();

    let lookup_tables_keys = TaskStrategist::collect_lookup_table_keys(
        &fixture.authority.pubkey(),
        &tasks,
    );
    let mut strategy = TransactionStrategy {
        optimized_tasks: tasks,
        lookup_tables_keys,
    };

    let result = preparator
        .prepare_for_delivery(
            &fixture.authority,
            &mut strategy,
            &None::<IntentPersisterImpl>,
        )
        .await;
    assert!(result.is_ok(), "Failed to prepare lookup tables");

    let alts = result.unwrap();
    // Verify the ALTs were actually created
    for alt in alts {
        let alt_account = fixture
            .rpc_client
            .get_account(&alt.key)
            .await
            .unwrap()
            .expect("ALT account should exist");

        assert!(!alt_account.data.is_empty(), "ALT account should have data");
    }
}

#[tokio::test]
async fn test_already_initialized_error_handled() {
    let fixture = TestFixture::new().await;
    let preparator = fixture.create_delivery_preparator();

    let data = generate_random_bytes(10 * 1024);
    let mut task = create_commit_task(&data);
    let buffer_task = BufferTaskType::Commit(task.clone());
    let mut strategy = TransactionStrategy {
        optimized_tasks: vec![Box::new(BufferTask::new_preparation_required(
            buffer_task,
        ))],
        lookup_tables_keys: vec![],
    };

    // Test preparation
    let result = preparator
        .prepare_for_delivery(
            &fixture.authority,
            &mut strategy,
            &None::<IntentPersisterImpl>,
        )
        .await;
    assert!(result.is_ok(), "Preparation failed: {:?}", result.err());

    // Verify the buffer account was created and initialized
    let PreparationState::Cleanup(cleanup_task) =
        strategy.optimized_tasks[0].preparation_state()
    else {
        panic!("unexpected PreparationState");
    };
    // Check buffer account exists
    let buffer_pda = cleanup_task.buffer_pda(&fixture.authority.pubkey());
    let account = fixture
        .rpc_client
        .get_account(&buffer_pda)
        .await
        .unwrap()
        .expect("Buffer account should exist");
    assert_eq!(account.data.as_slice(), data, "Unexpected account data");

    // Imitate commit to the non deleted buffer using different length
    // Keep same task with commit id, swap data
    let data =
        generate_random_bytes(task.committed_account.account.data.len() - 2);
    task.committed_account.account.data = data.clone();
    let buffer_task = BufferTaskType::Commit(task);
    let mut strategy = TransactionStrategy {
        optimized_tasks: vec![Box::new(BufferTask::new_preparation_required(
            buffer_task,
        ))],
        lookup_tables_keys: vec![],
    };

    // Test preparation
    let result = preparator
        .prepare_for_delivery(
            &fixture.authority,
            &mut strategy,
            &None::<IntentPersisterImpl>,
        )
        .await;
    assert!(result.is_ok(), "Preparation failed: {:?}", result.err());

    // Verify the buffer account was created and initialized
    let PreparationState::Cleanup(cleanup_task) =
        strategy.optimized_tasks[0].preparation_state()
    else {
        panic!("unexpected PreparationState");
    };

    // Check buffer account exists
    let buffer_pda = cleanup_task.buffer_pda(&fixture.authority.pubkey());
    let account = fixture
        .rpc_client
        .get_account(&buffer_pda)
        .await
        .unwrap()
        .expect("Buffer account should exist");
    assert_eq!(account.data.as_slice(), data, "Unexpected account data");
}
