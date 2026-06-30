use std::time::Duration;

use hydra_api::CRANKER_REWARD;
use integration_test_tools::{expect, validator::cleanup};
use magicblock_task_scheduler::crank_pubkey;
use program_flexi_counter::{
    instruction::{create_cancel_task_ix, create_schedule_task_ix},
    state::FlexiCounter,
};
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, signature::Keypair, signer::Signer,
    transaction::Transaction,
};
use test_task_scheduler::{
    create_delegated_counter, setup_validator, wait_for_hydra_crank,
    wait_for_hydra_crank_closed,
};

#[test]
fn test_schedule_task() {
    let (_temp_dir, mut validator, ctx) = setup_validator();

    let payer = Keypair::new();
    let (_counter_pda, _) = FlexiCounter::pda(&payer.pubkey());

    expect!(
        ctx.airdrop_chain(&payer.pubkey(), 10 * LAMPORTS_PER_SOL),
        validator
    );

    create_delegated_counter(&ctx, &payer, &mut validator, 0);

    let ephem_blockhash =
        expect!(ctx.try_get_latest_blockhash_ephem(), validator);

    // Schedule a task
    let task_id = 1;
    let execution_interval_millis = 100;
    let iterations = 3;
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

    // The crank is created by hydra and funded for every iteration.
    let crank_pda = crank_pubkey(&payer.pubkey(), task_id);
    let expected_lamports = iterations as u64 * CRANKER_REWARD;
    wait_for_hydra_crank(
        &ctx,
        &crank_pda,
        expected_lamports,
        Duration::from_secs(10),
        &mut validator,
    );

    // Cancel the task
    let sig = expect!(
        ctx.send_transaction_ephem_with_preflight(
            &mut Transaction::new_signed_with_payer(
                &[create_cancel_task_ix(payer.pubkey(), task_id,)],
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

    // Cancelling closes the hydra crank.
    wait_for_hydra_crank_closed(
        &ctx,
        &crank_pda,
        Duration::from_secs(10),
        &mut validator,
    );

    cleanup(&mut validator);
}
