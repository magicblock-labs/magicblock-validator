// solana-test-validator \
// --bpf-program corabpNrkBEqbTZP7xfJgSWTdBmVdLf1PARWXZbcMcS \
// ./magicblock-committor-program/bin/magicblock_committor_program.so

use borsh::BorshDeserialize;
use futures_util::StreamExt;
use magicblock_committor_program::Chunks;
use magicblock_committor_service::{
    persist::IntentPersisterImpl,
    tasks::{
        task_strategist::{TaskStrategist, TransactionStrategy},
        tasks::{ArgsTask, BaseTask, BufferTask},
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
    let buffer_task = BufferTask::Commit(create_commit_task(&data));
    let strategy = TransactionStrategy {
        optimized_tasks: vec![Box::new(buffer_task)],
        lookup_tables_keys: vec![],
    };

    // Test preparation
    let result = preparator
        .prepare_for_delivery(
            &fixture.authority,
            &strategy,
            &None::<IntentPersisterImpl>,
        )
        .await;

    assert!(result.is_ok(), "Preparation failed: {:?}", result.err());

    // Verify the buffer account was created and initialized
    let preparation_info = strategy.optimized_tasks[0]
        .preparation_info(&fixture.authority.pubkey())
        .expect("Task should have preparation info");

    // Check buffer account exists
    let buffer_account = fixture
        .rpc_client
        .get_account(&preparation_info.buffer_pda)
        .await
        .unwrap()
        .expect("Buffer account should exist");

    assert_eq!(buffer_account.data, data, "Buffer account size mismatch");

    // Check chunks account exists
    let chunks_account = fixture
        .rpc_client
        .get_account(&preparation_info.chunks_pda)
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

    let datas = vec![
        generate_random_bytes(10 * 1024),
        generate_random_bytes(10),
        generate_random_bytes(500 * 1024),
    ];
    let buffer_tasks = datas
        .iter()
        .map(|data| {
            let task = BufferTask::Commit(create_commit_task(data.as_slice()));
            Box::new(task) as Box<dyn BaseTask>
        })
        .collect();
    let strategy = TransactionStrategy {
        optimized_tasks: buffer_tasks,
        lookup_tables_keys: vec![],
    };

    // Test preparation
    let result = preparator
        .prepare_for_delivery(
            &fixture.authority,
            &strategy,
            &None::<IntentPersisterImpl>,
        )
        .await;

    assert!(result.is_ok(), "Preparation failed: {:?}", result.err());

    // Verify the buffer account was created and initialized
    let preparation_infos = strategy.optimized_tasks.iter().map(|el| {
        el.preparation_info(&fixture.authority.pubkey())
            .expect("Task should have preparation info")
    });

    for (i, preparation_info) in preparation_infos.enumerate() {
        // Check buffer account exists
        let buffer_account = fixture
            .rpc_client
            .get_account(&preparation_info.buffer_pda)
            .await
            .unwrap()
            .expect("Buffer account should exist");

        assert_eq!(
            buffer_account.data, datas[i],
            "Buffer account size mismatch"
        );

        // Check chunks account exists
        let chunks_account = fixture
            .rpc_client
            .get_account(&preparation_info.chunks_pda)
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

    let datas = vec![
        generate_random_bytes(10),
        generate_random_bytes(20),
        generate_random_bytes(30),
    ];
    let tasks = datas
        .iter()
        .map(|data| {
            let task = ArgsTask::Commit(create_commit_task(data.as_slice()));
            Box::new(task) as Box<dyn BaseTask>
        })
        .collect::<Vec<_>>();

    let lookup_tables_keys = TaskStrategist::collect_lookup_table_keys(
        &fixture.authority.pubkey(),
        &tasks,
    );
    let strategy = TransactionStrategy {
        optimized_tasks: tasks,
        lookup_tables_keys,
    };

    let result = preparator
        .prepare_for_delivery(
            &fixture.authority,
            &strategy,
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
