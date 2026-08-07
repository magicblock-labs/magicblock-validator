use std::time::Duration;

use hydra_api::consts::ephemeral::CRANKER_REWARD;
use integration_test_tools::{expect, validator::cleanup};
use magicblock_task_scheduler::{crank::crank_rent_floor, crank_pubkey};
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, signature::Keypair, signer::Signer,
};
use test_task_scheduler::{
    cancel_task, noop_task_instructions, schedule_noop_task, setup_validator,
    wait_for_funded_sponsor, wait_for_hydra_crank, wait_for_hydra_crank_closed,
};

/// Hydra cranks are sponsored by the validator identity. Scheduling moves
/// lamports out of it to fund a crank, and cancelling must return them, so a
/// validator that schedules and cancels tasks does not bleed itself dry over
/// time.
#[test]
fn test_sponsor_is_refunded_when_a_task_is_cancelled() {
    let (_temp_dir, mut validator, ctx, sponsor) = setup_validator();

    let payer = Keypair::new();
    expect!(
        ctx.airdrop_chain(&payer.pubkey(), 10 * LAMPORTS_PER_SOL),
        validator
    );

    // Sample the sponsor only once it carries a balance in the ER, otherwise
    // the baseline races validator startup.
    let sponsor_before = wait_for_funded_sponsor(
        &ctx,
        &sponsor,
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
        expect!(crank_rent_floor(&noop_task_instructions()), validator)
            .saturating_add(reward_pool);
    assert!(
        crank_lamports >= min_funding,
        "crank underfunded by sponsor: {crank_lamports} < {min_funding}"
    );

    // That funding came out of the sponsor, so the refund asserted below is a
    // real recovery rather than a vacuous no-op.
    let sponsor_while_scheduled =
        expect!(ctx.fetch_ephem_account_balance(&sponsor), validator);
    assert!(
        sponsor_while_scheduled < sponsor_before,
        "sponsor did not pay for the crank: {sponsor_while_scheduled} >= {sponsor_before}"
    );

    cancel_task(&ctx, &mut validator, &payer, task_id);
    wait_for_hydra_crank_closed(
        &ctx,
        &crank_pda,
        Duration::from_secs(10),
        &mut validator,
    );

    // Cancelling drains the crank back to the sponsor and refunds its rent, so
    // the sponsor ends up whole again apart from transaction fees.
    let sponsor_after =
        expect!(ctx.fetch_ephem_account_balance(&sponsor), validator);
    assert!(
        sponsor_after > sponsor_while_scheduled,
        "cancelling did not refund the crank budget: {sponsor_after} <= {sponsor_while_scheduled}"
    );
    assert!(
        sponsor_after + reward_pool >= sponsor_before,
        "sponsor was drained across a schedule/cancel cycle: {sponsor_after} vs {sponsor_before}"
    );

    cleanup(&mut validator);
}
