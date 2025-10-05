use cleanass::{assert, assert_eq};
use integration_test_tools::{expect, validator::cleanup};
use magicblock_program::{ID as MAGIC_PROGRAM_ID, TASK_CONTEXT_PUBKEY};
use magicblock_task_scheduler::SchedulerDatabase;
use program_flexi_counter::{
    instruction::{create_cancel_task_ix, create_schedule_task_ix},
    state::FlexiCounter,
};
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, signature::Keypair, signer::Signer,
    transaction::Transaction,
};
use test_task_scheduler::{
    create_delegated_counter, send_memo_tx, setup_validator,
};

#[test]
fn test_cancel_ongoing_task() {
    let (temp_dir, mut validator, ctx) = setup_validator();
    let db_path = SchedulerDatabase::path(temp_dir.path());

    let payer = Keypair::new();
    let (counter_pda, _) = FlexiCounter::pda(&payer.pubkey());

    expect!(
        ctx.airdrop_chain(&payer.pubkey(), 10 * LAMPORTS_PER_SOL),
        validator
    );

    create_delegated_counter(&ctx, &payer, &mut validator);

    // Noop tx to make sure the noop program is cloned
    let ephem_blockhash = send_memo_tx(&ctx, &payer, &mut validator);

    // Schedule a task
    let task_id = 3;
    let execution_interval_millis = 100;
    let iterations = 1000000;
    let sig = expect!(
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
    expect!(ctx.wait_for_delta_slot_ephem(2), validator);

    // Cancel the task
    let sig = expect!(
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
    let status = expect!(ctx.get_transaction_ephem(&sig), validator);
    expect!(
        status
            .transaction
            .meta
            .and_then(|m| m.status.ok())
            .ok_or_else(|| anyhow::anyhow!("Transaction failed")),
        validator
    );

    // Wait for the task to be cancelled
    expect!(ctx.wait_for_delta_slot_ephem(5), validator);

    // Check that the task was cancelled
    let db = expect!(SchedulerDatabase::new(db_path), validator);

    let failed_scheduling = expect!(db.get_failed_schedulings(), validator);
    assert_eq!(
        failed_scheduling.len(),
        0,
        cleanup(&mut validator),
        "failed_scheduling: {:?}",
        failed_scheduling,
    );

    let failed_tasks = expect!(db.get_failed_tasks(), validator);
    assert_eq!(
        failed_tasks.len(),
        0,
        cleanup(&mut validator),
        "failed_tasks: {:?}",
        failed_tasks
    );

    let tasks = expect!(db.get_task_ids(), validator);
    assert_eq!(
        tasks.len(),
        0,
        cleanup(&mut validator),
        "tasks: {:?}",
        tasks
    );

    let task = expect!(db.get_task(task_id), validator);
    assert!(task.is_none(), cleanup(&mut validator));

    // Check that the counter was incremented but not as much as the number of executions
    let counter_account = expect!(
        ctx.try_ephem_client().and_then(|client| client
            .get_account(&counter_pda)
            .map_err(|e| anyhow::anyhow!("Failed to get account: {}", e))),
        validator
    );
    let counter =
        expect!(FlexiCounter::try_decode(&counter_account.data), validator);
    assert!(
        counter.count < iterations,
        cleanup(&mut validator),
        "counter.count: {}",
        counter.count,
    );
    assert!(
        counter.count > 0,
        cleanup(&mut validator),
        "counter.count: {}",
        counter.count,
    );

    cleanup(&mut validator);
}
