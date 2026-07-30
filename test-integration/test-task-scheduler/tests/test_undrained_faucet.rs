use std::time::Duration;

use hydra_api::consts::ephemeral::CRANKER_REWARD;
use integration_test_tools::{expect, validator::cleanup};
use magicblock_task_scheduler::{crank::crank_rent_floor, crank_pubkey};
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, signature::Keypair, signer::Signer,
};
use test_task_scheduler::{
    cancel_task, noop_task_instructions, schedule_noop_task, setup_validator,
    wait_for_funded_faucet, wait_for_hydra_crank, wait_for_hydra_crank_closed,
};

/// Hydra cranks are sponsored by a dedicated, delegated faucet — not the
/// validator identity. Scheduling moves lamports out of the faucet to fund a
/// crank, and cancelling must return them, so a validator that schedules and
/// cancels tasks does not bleed its faucet dry over time.
#[test]
fn test_faucet_is_refunded_when_a_task_is_cancelled() {
    let (_temp_dir, mut validator, ctx, faucet) = setup_validator();
    let faucet_pk = faucet.pubkey();

    let payer = Keypair::new();
    expect!(
        ctx.airdrop_chain(&payer.pubkey(), 10 * LAMPORTS_PER_SOL),
        validator
    );

    // Sample the faucet only once it is delegated into the ER, otherwise the
    // baseline races the validator's background delegation.
    let faucet_before = wait_for_funded_faucet(
        &ctx,
        &faucet_pk,
        Duration::from_secs(30),
        &mut validator,
    );

    let task_id = 1;
    let iterations = 3;
    schedule_noop_task(&ctx, &mut validator, &payer, task_id, 100, iterations);

    let crank_pda = crank_pubkey(&payer.pubkey(), task_id);
    wait_for_hydra_crank(
        &ctx,
        &crank_pda,
        Duration::from_secs(10),
        &mut validator,
    );

    // The scheduler must pre-fund the crank with its rent *and* the whole
    // reward pool, or the last iterations would never be worth cranking.
    // Checking the reward pool on its own would pass on rent alone, since rent
    // dwarfs the per-trigger reward.
    let crank_lamports =
        expect!(ctx.fetch_ephem_account_balance(&crank_pda), validator);
    let reward_pool = (iterations as u64).saturating_mul(CRANKER_REWARD);
    let min_funding =
        crank_rent_floor(&noop_task_instructions()).saturating_add(reward_pool);
    assert!(
        crank_lamports >= min_funding,
        "crank underfunded by faucet: {crank_lamports} < {min_funding}"
    );

    // That funding came out of the faucet, so the refund asserted below is a
    // real recovery rather than a vacuous no-op.
    let faucet_while_scheduled =
        expect!(ctx.fetch_ephem_account_balance(&faucet_pk), validator);
    assert!(
        faucet_while_scheduled < faucet_before,
        "faucet did not pay for the crank: {faucet_while_scheduled} >= {faucet_before}"
    );

    cancel_task(&ctx, &mut validator, &payer, task_id);
    wait_for_hydra_crank_closed(
        &ctx,
        &crank_pda,
        Duration::from_secs(10),
        &mut validator,
    );

    // Cancelling drains the crank back to the faucet and refunds its rent, so
    // the faucet ends up whole again apart from transaction fees.
    let faucet_after =
        expect!(ctx.fetch_ephem_account_balance(&faucet_pk), validator);
    assert!(
        faucet_after > faucet_while_scheduled,
        "cancelling did not refund the crank budget: {faucet_after} <= {faucet_while_scheduled}"
    );
    assert!(
        faucet_after + reward_pool >= faucet_before,
        "faucet was drained across a schedule/cancel cycle: {faucet_after} vs {faucet_before}"
    );

    cleanup(&mut validator);
}
