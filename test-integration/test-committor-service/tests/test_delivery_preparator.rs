use std::sync::Arc;

use borsh::BorshDeserialize;
use compressed_delegation_client::CompressedDelegationRecord;
use light_client::indexer::photon_indexer::PhotonIndexer;
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
use magicblock_program::validator::{
    generate_validator_authority_if_needed, validator_authority_id,
};
use solana_sdk::{
    rent::Rent, signature::Keypair, signer::Signer, sysvar::Sysvar,
};
use test_kit::init_logger;

use crate::common::{
    create_commit_task, create_compressed_commit_task, generate_random_bytes,
    TestFixture,
};
use crate::utils::transactions::init_and_delegate_compressed_account_on_chain;

mod common;
mod utils;

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
            &None::<Arc<PhotonIndexer>>,
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
            &None::<Arc<PhotonIndexer>>,
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
            &None::<Arc<PhotonIndexer>>,
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
async fn test_prepare_compressed_commit() {
    let fixture = TestFixture::new().await;
    let preparator = fixture.create_delivery_preparator();

    generate_validator_authority_if_needed();
    init_logger!();

    let counter_auth = Keypair::new();
    let (pda, _hash, account) =
        init_and_delegate_compressed_account_on_chain(&counter_auth).await;

    let data = generate_random_bytes(10);
    let mut task = Box::new(ArgsTask::new(ArgsTaskType::CompressedCommit(
        create_compressed_commit_task(pda, Default::default(), data.as_slice()),
    ))) as Box<dyn BaseTask>;
    let compressed_data = task.as_ref().get_compressed_data().clone();

    preparator
        .prepare_task(
            &fixture.authority,
            &mut *task,
            &None::<IntentPersisterImpl>,
            &Some(fixture.photon_client),
        )
        .await
        .expect("Failed to prepare compressed commit");

    // Verify the compressed data was updated
    let new_compressed_data = task.as_ref().get_compressed_data();
    assert_ne!(
        new_compressed_data, compressed_data,
        "Compressed data size mismatch"
    );

    // Verify the delegation record is correct
    let delegation_record = CompressedDelegationRecord::from_bytes(
        &new_compressed_data
            .unwrap()
            .compressed_delegation_record_bytes,
    )
    .unwrap();
    let expected = CompressedDelegationRecord {
        authority: validator_authority_id(),
        pda,
        delegation_slot: delegation_record.delegation_slot,
        lamports: Rent::default().minimum_balance(account.data.len()),
        data: account.data,
        last_update_nonce: 0,
        is_undelegatable: false,
        owner: program_flexi_counter::ID,
    };
    assert_eq!(
        delegation_record, expected,
        "Delegation record should be the same"
    );
}
