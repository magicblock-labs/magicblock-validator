use std::time::{Duration, Instant};

use cleanass::{assert, assert_eq};
use integration_test_tools::{expect, validator::cleanup};
use magicblock_task_scheduler::SchedulerDatabase;
use program_flexi_counter::instruction::create_schedule_task_ix;
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, signature::Keypair, signer::Signer,
    transaction::Transaction,
};
use test_task_scheduler::{
    create_delegated_counter, setup_validator,
};
use tokio::runtime::Runtime;

#[test]
fn test_unauthorized_reschedule() {
    let (temp_dir, mut validator, ctx) = setup_validator();
    let db_path = SchedulerDatabase::path(temp_dir.path());

    let payer = Keypair::new();
    let different_payer = Keypair::new();

    expect!(
        ctx.airdrop_chain(&payer.pubkey(), 10 * LAMPORTS_PER_SOL),
        validator
    );
    expect!(
        ctx.airdrop_chain(&different_payer.pubkey(), 10 * LAMPORTS_PER_SOL),
        validator
    );

    create_delegated_counter(&ctx, &payer, &mut validator, 0);
    create_delegated_counter(&ctx, &different_payer, &mut validator, 0);

    let ephem_blockhash =
        expect!(ctx.try_get_latest_blockhash_ephem(), validator);

    // Schedule a task
    let task_id = 1;
    let execution_interval_millis = 100;
    let iterations = 1000;
    let sig = expect!(
        ctx.send_transaction_ephem_with_preflight(
            &mut Transaction::new_signed_with_payer(
                &[create_schedule_task_ix(
                    payer.pubkey(),
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
    let status = expect!(ctx.get_transaction_ephem(&sig), validator);
    expect!(
        status
            .transaction
            .meta
            .and_then(|m| m.status.ok())
            .ok_or_else(|| anyhow::anyhow!("Transaction failed")),
        validator
    );

    // Wait for the task to be scheduled
    expect!(ctx.wait_for_delta_slot_ephem(5), validator);

    // Reschedule the same task with a different payer
    let new_execution_interval_millis = 200;
    let sig = expect!(
        ctx.send_transaction_ephem_with_preflight(
            &mut Transaction::new_signed_with_payer(
                &[create_schedule_task_ix(
                    different_payer.pubkey(),
                    task_id,
                    new_execution_interval_millis,
                    iterations,
                    false,
                    false,
                )],
                Some(&different_payer.pubkey()),
                &[&different_payer],
                ephem_blockhash,
            ),
            &[&different_payer]
        ),
        validator
    );
    let status = expect!(ctx.get_transaction_ephem(&sig), validator);
    expect!(
        status
            .transaction
            .meta
            .and_then(|m| m.status.ok())
            .ok_or_else(|| anyhow::anyhow!("Transaction failed")),
        validator
    );

    // Check that one task is scheduled but another one is failed to schedule
    let db = expect!(SchedulerDatabase::new(db_path), validator);
    let runtime = expect!(Runtime::new(), validator);

    let failed_scheduling = wait_for_failed_schedulings(
        &db,
        &runtime,
        1,
        Duration::from_secs(10),
        &ctx,
        &mut validator,
    );
    assert_eq!(
        failed_scheduling.len(),
        1,
        cleanup(&mut validator),
        "failed_scheduling: {:?}",
        failed_scheduling,
    );
    assert_eq!(
        failed_scheduling[0].task_id, task_id,
        cleanup(&mut validator),
        "failed_scheduling: {:?}", failed_scheduling,
    );
    assert!(
        failed_scheduling[0].error.contains("UnauthorizedReplacing"),
        cleanup(&mut validator),
        "failed_scheduling: {:?}",
        failed_scheduling,
    );

    let failed_tasks =
        expect!(runtime.block_on(db.get_failed_tasks()), validator);
    assert_eq!(
        failed_tasks.len(),
        0,
        cleanup(&mut validator),
        "failed_tasks: {:?}",
        failed_tasks
    );

    let tasks = expect!(runtime.block_on(db.get_task_ids()), validator);
    assert_eq!(tasks.len(), 1, cleanup(&mut validator));

    let task = expect!(
        runtime
            .block_on(db.get_task(task_id))
            .ok()
            .flatten()
            .ok_or(anyhow::anyhow!("Task not found")),
        validator
    );
    assert_eq!(task.authority, payer.pubkey(), cleanup(&mut validator));
    assert_eq!(
        task.execution_interval_millis,
        execution_interval_millis,
        cleanup(&mut validator)
    );
    assert!(
        task.executions_left > 0,
        cleanup(&mut validator),
        "task.executions_left: {}",
        task.executions_left
    );

    cleanup(&mut validator);
}

fn wait_for_failed_schedulings(
    db: &SchedulerDatabase,
    runtime: &Runtime,
    expected_len: usize,
    timeout: Duration,
    ctx: &integration_test_tools::IntegrationTestContext,
    validator: &mut std::process::Child,
) -> Vec<magicblock_task_scheduler::db::FailedScheduling> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        let failed_scheduling =
            expect!(runtime.block_on(db.get_failed_schedulings()), validator);
        if failed_scheduling.len() == expected_len {
            return failed_scheduling;
        }

        expect!(ctx.wait_for_next_slot_ephem(), validator);
    }

    let failed_scheduling =
        expect!(runtime.block_on(db.get_failed_schedulings()), validator);
    assert_eq!(
        failed_scheduling.len(),
        expected_len,
        cleanup(validator),
        "failed_scheduling: {:?}",
        failed_scheduling,
    );
    failed_scheduling
}
