use std::thread::sleep;

use cleanass::{assert, assert_eq};
use integration_test_tools::{expect, validator::cleanup};
use magicblock_program::{ID as MAGIC_PROGRAM_ID, TASK_CONTEXT_PUBKEY};
use magicblock_task_scheduler::SchedulerDatabase;
use program_flexi_counter::{
    instruction::{
        create_cancel_task_ix, create_delegate_ix, create_init_ix,
        create_schedule_task_ix,
    },
    state::FlexiCounter,
};
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, signature::Keypair, signer::Signer,
    transaction::Transaction,
};
use test_task_scheduler::setup_validator;

#[test]
fn test_schedule_task() {
    let (temp_dir, mut validator, ctx) = setup_validator();
    let db_path = SchedulerDatabase::path(temp_dir.path());

    let payer = Keypair::new();
    let (counter_pda, _) = FlexiCounter::pda(&payer.pubkey());

    expect!(
        ctx.airdrop_chain(&payer.pubkey(), 10 * LAMPORTS_PER_SOL),
        validator
    );

    // Initialize the counter
    let blockhash = expect!(
        ctx.try_chain_client().and_then(|client| client
            .get_latest_blockhash()
            .map_err(|e| anyhow::anyhow!(
                "Failed to get latest blockhash: {}",
                e
            ))),
        validator
    );
    expect!(
        ctx.send_transaction_chain(
            &mut Transaction::new_signed_with_payer(
                &[create_init_ix(payer.pubkey(), "test".to_string())],
                Some(&payer.pubkey()),
                &[&payer],
                blockhash,
            ),
            &[&payer]
        ),
        validator
    );

    // Delegate the counter to the ephem validator
    expect!(
        ctx.send_transaction_chain(
            &mut Transaction::new_signed_with_payer(
                &[create_delegate_ix(payer.pubkey())],
                Some(&payer.pubkey()),
                &[&payer],
                blockhash,
            ),
            &[&payer]
        ),
        validator
    );

    // Wait for account to be delegated
    sleep(std::time::Duration::from_secs(3));

    // Schedule a task
    let task_id = 1;
    let execution_interval_millis = 100;
    let iterations = 3;
    let ephem_blockhash = expect!(
        ctx.try_ephem_client().and_then(|client| client
            .get_latest_blockhash()
            .map_err(|e| anyhow::anyhow!(
                "Failed to get latest blockhash: {}",
                e
            ))),
        validator
    );

    expect!(
        ctx.send_transaction_ephem(
            &mut Transaction::new_signed_with_payer(
                &[create_schedule_task_ix(
                    payer.pubkey(),
                    TASK_CONTEXT_PUBKEY,
                    MAGIC_PROGRAM_ID,
                    task_id,
                    execution_interval_millis,
                    iterations,
                    false,
                    false,
                )],
                Some(&payer.pubkey()),
                &[&payer],
                ephem_blockhash,
            ),
            &[&payer]
        ),
        validator
    );

    // Wait for the task to be scheduled
    sleep(std::time::Duration::from_secs(3));

    // Check that the task was scheduled in the database
    let db = expect!(SchedulerDatabase::new(db_path), validator);

    let failed_scheduling = expect!(db.get_failed_scheduling_ids(), validator);
    assert_eq!(
        failed_scheduling.len(),
        0,
        cleanup(&mut validator),
        "failed_scheduling: {:?}",
        failed_scheduling,
    );

    let failed_tasks = expect!(db.get_failed_task_ids(), validator);
    assert_eq!(
        failed_tasks.len(),
        0,
        cleanup(&mut validator),
        "failed_tasks: {:?}",
        failed_tasks
    );

    let tasks = expect!(db.get_task_ids(), validator);
    assert_eq!(tasks.len(), 1, cleanup(&mut validator));

    let task = expect!(
        db.get_task(task_id)
            .ok()
            .flatten()
            .ok_or(anyhow::anyhow!("Task not found")),
        validator
    );
    assert_eq!(task.id, task_id, cleanup(&mut validator));
    assert_eq!(task.authority, payer.pubkey(), cleanup(&mut validator));
    assert_eq!(task.execution_interval_millis, 100, cleanup(&mut validator));
    assert_eq!(task.executions_left, 0, cleanup(&mut validator));

    // Check that the counter was incremented
    let counter_account = expect!(
        ctx.try_ephem_client().and_then(|client| client
            .get_account(&counter_pda)
            .map_err(|e| anyhow::anyhow!("Failed to get account: {}", e))),
        validator
    );
    let counter =
        expect!(FlexiCounter::try_decode(&counter_account.data), validator);
    assert!(
        counter.count == iterations,
        cleanup(&mut validator),
        "counter.count: {}",
        counter.count
    );

    // Cancel the task
    expect!(
        ctx.send_transaction_ephem(
            &mut Transaction::new_signed_with_payer(
                &[create_cancel_task_ix(
                    payer.pubkey(),
                    TASK_CONTEXT_PUBKEY,
                    MAGIC_PROGRAM_ID,
                    task_id,
                )],
                Some(&payer.pubkey()),
                &[&payer],
                ephem_blockhash,
            ),
            &[&payer]
        ),
        validator
    );

    sleep(std::time::Duration::from_secs(3));

    // Check that the task was cancelled
    let tasks = expect!(db.get_task_ids(), validator);
    assert_eq!(tasks.len(), 0, cleanup(&mut validator));

    cleanup(&mut validator);
}
