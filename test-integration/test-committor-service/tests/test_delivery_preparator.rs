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

#[tokio::test]
async fn test_prepare_cleanup_and_reprepare_mixed_tasks() {
    use borsh::BorshDeserialize;

    let fixture = TestFixture::new().await;
    let preparator = fixture.create_delivery_preparator();

    // Data of committed accs
    let args_data = generate_random_bytes(33);
    let buf_a_data = generate_random_bytes(12 * 1024);
    let buf_b_data = generate_random_bytes(64 * 1024 + 3);

    // Keep these around to modify data later (same commit IDs, different data)
    let mut commit_args = create_commit_task(&args_data);
    let mut commit_a = create_commit_task(&buf_a_data);
    let mut commit_b = create_commit_task(&buf_b_data);

    let mut strategy = TransactionStrategy {
        optimized_tasks: vec![
            // Args task — shouldn't need buffers
            {
                let t = ArgsTaskType::Commit(commit_args.clone());
                Box::<ArgsTask>::new(t.into()) as Box<dyn BaseTask>
            },
            // Two buffer tasks
            {
                let t = BufferTaskType::Commit(commit_a.clone());
                Box::new(BufferTask::new_preparation_required(t))
                    as Box<dyn BaseTask>
            },
            {
                let t = BufferTaskType::Commit(commit_b.clone());
                Box::new(BufferTask::new_preparation_required(t))
                    as Box<dyn BaseTask>
            },
        ],
        lookup_tables_keys: vec![],
    };

    // --- Step 1: initial prepare ---
    let res = preparator
        .prepare_for_delivery(
            &fixture.authority,
            &mut strategy,
            &None::<IntentPersisterImpl>,
        )
        .await;
    assert!(res.is_ok(), "Initial prepare failed: {:?}", res.err());

    // Collect cleanup states for the two buffer tasks, verify they wrote expected data+chunks
    let mut buffer_cleanups = Vec::new();
    for t in &strategy.optimized_tasks {
        if let PreparationState::Cleanup(c) = t.preparation_state() {
            buffer_cleanups.push(c);
        }
    }
    assert_eq!(
        buffer_cleanups.len(),
        2,
        "Expected exactly 2 buffer cleanup tasks"
    );

    // Map PDAs -> expected initial data
    // (Order corresponds to buffer tasks added to the strategy)
    let expected_initial_datas: [&[u8]; 2] = [&buf_a_data, &buf_b_data];
    for (i, c) in buffer_cleanups.iter().enumerate() {
        let buffer_pda = c.buffer_pda(&fixture.authority.pubkey());
        let chunks_pda = c.chunks_pda(&fixture.authority.pubkey());

        // Buffer content
        let acc = fixture
            .rpc_client
            .get_account(&buffer_pda)
            .await
            .unwrap()
            .expect("Buffer account should exist after initial prepare");
        assert_eq!(
            acc.data.as_slice(),
            expected_initial_datas[i],
            "Initial buffer data mismatch at index {}",
            i
        );

        // Chunks complete
        let chunks_acc = fixture
            .rpc_client
            .get_account(&chunks_pda)
            .await
            .unwrap()
            .expect("Chunks account should exist after initial prepare");
        let chunks = Chunks::try_from_slice(&chunks_acc.data)
            .expect("Failed to deserialize chunks");
        assert!(
            chunks.is_complete(),
            "Chunks should be complete after initial prepare (index {})",
            i
        );
    }

    // --- Step 2: simulate buffer reprepare and AccountAlreadyInitialized error
    if !commit_args.committed_account.account.data.is_empty() {
        commit_args.committed_account.account.data[0] ^= 0x01;
    }

    // Buffer A: change size a little (e.g., +5 bytes)
    {
        let d = &mut commit_a.committed_account.account.data;
        d.extend_from_slice(&[42, 43, 44, 45, 46]);
    }
    // Buffer B: shrink by 5 bytes
    {
        commit_b
            .committed_account
            .account
            .data
            .truncate(buf_b_data.len() - 5);
    }

    // --- Step 4: re-prepare with the same logical tasks (same commit IDs, mutated data) ---
    let mut strategy2 = TransactionStrategy {
        optimized_tasks: vec![
            {
                let t = ArgsTaskType::Commit(commit_args.clone());
                Box::<ArgsTask>::new(t.into()) as Box<dyn BaseTask>
            },
            {
                let t = BufferTaskType::Commit(commit_a.clone());
                Box::new(BufferTask::new_preparation_required(t))
                    as Box<dyn BaseTask>
            },
            {
                let t = BufferTaskType::Commit(commit_b.clone());
                Box::new(BufferTask::new_preparation_required(t))
                    as Box<dyn BaseTask>
            },
        ],
        lookup_tables_keys: vec![],
    };

    let res2 = preparator
        .prepare_for_delivery(
            &fixture.authority,
            &mut strategy2,
            &None::<IntentPersisterImpl>,
        )
        .await;
    assert!(
        res2.is_ok(),
        "Re-prepare failed after cleanup: {:?}",
        res2.err()
    );

    // Verify buffers reflect the *new* data and chunks are complete again
    let mut buffer_cleanups2 = Vec::new();
    for t in &strategy2.optimized_tasks {
        if let PreparationState::Cleanup(c) = t.preparation_state() {
            buffer_cleanups2.push(c);
        }
    }
    assert_eq!(
        buffer_cleanups2.len(),
        2,
        "Expected 2 buffer cleanup tasks on re-prepare"
    );

    // Expected new datas match the mutated commits
    let expected_new_datas: [&[u8]; 2] = [
        &commit_a.committed_account.account.data,
        &commit_b.committed_account.account.data,
    ];

    for (i, c) in buffer_cleanups2.iter().enumerate() {
        let buffer_pda = c.buffer_pda(&fixture.authority.pubkey());
        let chunks_pda = c.chunks_pda(&fixture.authority.pubkey());

        // Buffer content should match mutated data
        let acc = fixture
            .rpc_client
            .get_account(&buffer_pda)
            .await
            .unwrap()
            .expect("Buffer account should exist after re-prepare");
        assert_eq!(
            acc.data.as_slice(),
            expected_new_datas[i],
            "Re-prepare buffer data mismatch at index {}",
            i
        );

        // Chunks complete again
        let chunks_acc = fixture
            .rpc_client
            .get_account(&chunks_pda)
            .await
            .unwrap()
            .expect("Chunks account should exist after re-prepare");
        let chunks = Chunks::try_from_slice(&chunks_acc.data)
            .expect("Failed to deserialize chunks");
        assert!(
            chunks.is_complete(),
            "Chunks should be complete after re-prepare (index {})",
            i
        );
    }
}
