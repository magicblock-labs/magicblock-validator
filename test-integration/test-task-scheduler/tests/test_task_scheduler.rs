use std::{path::PathBuf, thread::sleep};

use integration_test_tools::IntegrationTestContext;
use magicblock_program::{ID as MAGIC_PROGRAM_ID, TASK_CONTEXT_PUBKEY};
use magicblock_task_scheduler::SchedulerDatabase;
use schedule_task_program::{
    instruction::{
        create_cancel_task_ix, create_delegate_ix, create_init_counter_ix,
        create_schedule_task_ix,
    },
    state::Counter,
};
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, signature::Keypair, signer::Signer,
    transaction::Transaction,
};

const DB_PATH: &str = "../../target/test.db";

#[test]
fn test_task_scheduler() {
    let ctx = IntegrationTestContext::try_new().unwrap();
    let payer = Keypair::new();
    let (counter_pda, _) = Counter::pda(&payer.pubkey());

    ctx.airdrop_chain(&payer.pubkey(), 10 * LAMPORTS_PER_SOL)
        .unwrap();

    // Initialize the counter
    ctx.try_chain_client()
        .unwrap()
        .send_transaction(&Transaction::new_signed_with_payer(
            &[create_init_counter_ix(payer.pubkey())],
            Some(&payer.pubkey()),
            &[&payer],
            ctx.try_chain_client()
                .unwrap()
                .get_latest_blockhash()
                .unwrap(),
        ))
        .unwrap();

    // Delegate the counter to the ephem validator
    ctx.try_chain_client()
        .unwrap()
        .send_transaction(&Transaction::new_signed_with_payer(
            &[create_delegate_ix(payer.pubkey())],
            Some(&payer.pubkey()),
            &[&payer],
            ctx.try_chain_client()
                .unwrap()
                .get_latest_blockhash()
                .unwrap(),
        ))
        .unwrap();

    // Wait for account to be delegated
    sleep(std::time::Duration::from_secs(3));

    // Schedule a task
    let task_id = 1;
    let period_millis = 100;
    let n_executions = 3;
    ctx.try_ephem_client()
        .unwrap()
        .send_transaction(&Transaction::new_signed_with_payer(
            &[create_schedule_task_ix(
                payer.pubkey(),
                TASK_CONTEXT_PUBKEY,
                MAGIC_PROGRAM_ID,
                task_id,
                period_millis,
                n_executions,
                false,
            )],
            Some(&payer.pubkey()),
            &[&payer],
            ctx.try_ephem_client()
                .unwrap()
                .get_latest_blockhash()
                .unwrap(),
        ))
        .unwrap();

    // Wait for the task to be scheduled
    sleep(std::time::Duration::from_secs(3));

    // Check that the task was scheduled in the database
    let db_path = PathBuf::from(DB_PATH);
    let db = SchedulerDatabase::new(db_path).unwrap();

    let tasks = db.get_task_ids().unwrap();
    assert_eq!(tasks.len(), 1);

    let task = db.get_task(task_id).unwrap().unwrap();
    assert_eq!(task.id, task_id);
    assert_eq!(task.authority, payer.pubkey());
    assert_eq!(task.period_millis, 100);
    assert_eq!(task.executions_left, 0);

    // Check that the counter was incremented
    let counter_account = ctx
        .try_ephem_client()
        .unwrap()
        .get_account(&counter_pda)
        .unwrap();
    let counter = Counter::try_decode(&counter_account.data).unwrap();
    assert!(
        counter.count == n_executions,
        "counter.count: {}",
        counter.count
    );

    // Cancel the task
    ctx.try_ephem_client()
        .unwrap()
        .send_transaction(&Transaction::new_signed_with_payer(
            &[create_cancel_task_ix(
                payer.pubkey(),
                TASK_CONTEXT_PUBKEY,
                MAGIC_PROGRAM_ID,
                task_id,
            )],
            Some(&payer.pubkey()),
            &[&payer],
            ctx.try_ephem_client()
                .unwrap()
                .get_latest_blockhash()
                .unwrap(),
        ))
        .unwrap();

    sleep(std::time::Duration::from_secs(3));

    // Check that the task was cancelled
    let tasks = db.get_task_ids().unwrap();
    assert_eq!(tasks.len(), 0);
}

// Test that a task with an error is unscheduled
#[test]
fn test_task_scheduler_error() {
    let ctx = IntegrationTestContext::try_new().unwrap();
    let payer = Keypair::new();
    let (counter_pda, _) = Counter::pda(&payer.pubkey());

    ctx.airdrop_chain(&payer.pubkey(), 10 * LAMPORTS_PER_SOL)
        .unwrap();

    // Initialize the counter
    ctx.try_chain_client()
        .unwrap()
        .send_transaction(&Transaction::new_signed_with_payer(
            &[create_init_counter_ix(payer.pubkey())],
            Some(&payer.pubkey()),
            &[&payer],
            ctx.try_chain_client()
                .unwrap()
                .get_latest_blockhash()
                .unwrap(),
        ))
        .unwrap();

    // Delegate the counter to the ephem validator
    ctx.try_chain_client()
        .unwrap()
        .send_transaction(&Transaction::new_signed_with_payer(
            &[create_delegate_ix(payer.pubkey())],
            Some(&payer.pubkey()),
            &[&payer],
            ctx.try_chain_client()
                .unwrap()
                .get_latest_blockhash()
                .unwrap(),
        ))
        .unwrap();

    // Wait for account to be delegated
    sleep(std::time::Duration::from_secs(3));

    // Schedule a task
    let task_id = 2;
    let period_millis = 100;
    let n_executions = 3;
    ctx.try_ephem_client()
        .unwrap()
        .send_transaction(&Transaction::new_signed_with_payer(
            &[create_schedule_task_ix(
                payer.pubkey(),
                TASK_CONTEXT_PUBKEY,
                MAGIC_PROGRAM_ID,
                task_id,
                period_millis,
                n_executions,
                true,
            )],
            Some(&payer.pubkey()),
            &[&payer],
            ctx.try_ephem_client()
                .unwrap()
                .get_latest_blockhash()
                .unwrap(),
        ))
        .unwrap();

    // Wait for the task to be scheduled
    sleep(std::time::Duration::from_secs(3));

    // Check that the task was scheduled in the database
    let db_path = PathBuf::from(DB_PATH);
    let db = SchedulerDatabase::new(db_path).unwrap();

    let tasks = db.get_task_ids().unwrap();
    assert_eq!(tasks.len(), 1);

    let task = db.get_task(task_id).unwrap().unwrap();
    assert_eq!(task.id, task_id);
    assert_eq!(task.authority, payer.pubkey());
    assert_eq!(task.period_millis, 100);
    assert_eq!(task.executions_left, 0);
    assert_eq!(task.next_execution_millis, 0);

    // Check that the counter was not incremented
    let counter_account = ctx
        .try_ephem_client()
        .unwrap()
        .get_account(&counter_pda)
        .unwrap();
    let counter = Counter::try_decode(&counter_account.data).unwrap();
    assert!(counter.count == 0, "counter.count: {}", counter.count);

    // Cancel the task
    ctx.try_ephem_client()
        .unwrap()
        .send_transaction(&Transaction::new_signed_with_payer(
            &[create_cancel_task_ix(
                payer.pubkey(),
                TASK_CONTEXT_PUBKEY,
                MAGIC_PROGRAM_ID,
                task_id,
            )],
            Some(&payer.pubkey()),
            &[&payer],
            ctx.try_ephem_client()
                .unwrap()
                .get_latest_blockhash()
                .unwrap(),
        ))
        .unwrap();

    sleep(std::time::Duration::from_secs(3));

    // Check that the task was cancelled
    let tasks = db.get_task_ids().unwrap();
    assert_eq!(tasks.len(), 0);
}
