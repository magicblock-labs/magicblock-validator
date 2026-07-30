use std::time::Duration;

use integration_test_tools::{expect, validator::cleanup};
use magicblock_task_scheduler::crank_pubkey;
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, signature::Keypair, signer::Signer,
};
use test_task_scheduler::{
    cancel_task, schedule_noop_task, setup_validator, wait_for_hydra_crank,
    wait_for_hydra_crank_closed,
};

/// Scheduling a task makes the task scheduler create the hydra crank that will
/// run it, and cancelling the task makes it close that crank again.
#[test]
fn test_schedule_task_creates_and_cancels_hydra_crank() {
    let (_temp_dir, mut validator, ctx, _faucet) = setup_validator();

    let payer = Keypair::new();
    expect!(
        ctx.airdrop_chain(&payer.pubkey(), 10 * LAMPORTS_PER_SOL),
        validator
    );

    let task_id = 1;
    schedule_noop_task(&ctx, &mut validator, &payer, task_id, 100, 3);

    // The crank lives at a PDA derived from (authority, task_id), so the test
    // can locate it without asking the scheduler.
    let crank_pda = crank_pubkey(&payer.pubkey(), task_id);
    wait_for_hydra_crank(
        &ctx,
        &crank_pda,
        Duration::from_secs(10),
        &mut validator,
    );

    cancel_task(&ctx, &mut validator, &payer, task_id);

    wait_for_hydra_crank_closed(
        &ctx,
        &crank_pda,
        Duration::from_secs(10),
        &mut validator,
    );

    cleanup(&mut validator);
}

/// Each (authority, task_id) pair gets its own crank, so one authority
/// cancelling its task leaves another authority's task with the same id alone.
#[test]
fn test_tasks_are_namespaced_per_authority() {
    let (_temp_dir, mut validator, ctx, _faucet) = setup_validator();

    let payer = Keypair::new();
    let other = Keypair::new();
    for keypair in [&payer, &other] {
        expect!(
            ctx.airdrop_chain(&keypair.pubkey(), 10 * LAMPORTS_PER_SOL),
            validator
        );
    }

    // Deliberately the same task id for both authorities.
    let task_id = 7;
    schedule_noop_task(&ctx, &mut validator, &payer, task_id, 100, 3);
    schedule_noop_task(&ctx, &mut validator, &other, task_id, 100, 3);

    let payer_crank = crank_pubkey(&payer.pubkey(), task_id);
    let other_crank = crank_pubkey(&other.pubkey(), task_id);
    assert_ne!(
        payer_crank, other_crank,
        "same task id under different authorities must not share a crank"
    );

    for crank in [&payer_crank, &other_crank] {
        wait_for_hydra_crank(
            &ctx,
            crank,
            Duration::from_secs(10),
            &mut validator,
        );
    }

    cancel_task(&ctx, &mut validator, &payer, task_id);
    wait_for_hydra_crank_closed(
        &ctx,
        &payer_crank,
        Duration::from_secs(10),
        &mut validator,
    );

    // The other authority's crank is untouched by that cancellation.
    wait_for_hydra_crank(
        &ctx,
        &other_crank,
        Duration::from_secs(10),
        &mut validator,
    );

    cleanup(&mut validator);
}
