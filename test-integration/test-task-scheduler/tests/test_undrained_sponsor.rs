use std::time::Duration;

use cleanass::assert;
use hydra_api::state::{crank_account_size, region_len_for};
use integration_test_tools::{expect, validator::cleanup};
use magicblock_program::{
    ephemeral::rent_for, instruction_utils::InstructionUtils,
};
use magicblock_task_scheduler::crank_pubkey;
use solana_sdk::{signature::Keypair, signer::Signer};
use test_task_scheduler::{
    cancel_task, schedule_noop_task, setup_validator, wait_for_hydra_crank,
    wait_for_hydra_crank_closed,
};

/// Hydra cranks are sponsored by the validator identity. Scheduling moves
/// lamports out of it to fund a crank, and cancelling must return them, so a
/// validator that schedules and cancels tasks does not bleed itself dry over
/// time.
#[test]
fn test_sponsor_is_refunded_when_a_task_is_cancelled() {
    let (_temp_dir, mut validator, ctx, sponsor) = setup_validator();

    let sponsor_before =
        expect!(ctx.fetch_ephem_account_balance(&sponsor), validator);

    let payer = Keypair::new();

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

    let sponsor_while_scheduled =
        expect!(ctx.fetch_ephem_account_balance(&sponsor), validator);

    // The scheduler must pre-fund the crank with its rent
    let ix = InstructionUtils::noop_instruction(0);
    let min_funding = expect!(
        rent_for(crank_account_size(region_len_for(
            ix.accounts.len(),
            ix.data.len()
        )) as u32),
        validator
    );
    assert!(
        sponsor_while_scheduled == sponsor_before - min_funding,
        cleanup(&mut validator),
        "sponsor did not pre-fund the crank: {sponsor_while_scheduled} != {sponsor_before} - {min_funding}"
    );

    cancel_task(&ctx, &mut validator, &payer, task_id);
    wait_for_hydra_crank_closed(
        &ctx,
        &crank_pda,
        Duration::from_secs(10),
        &mut validator,
    );

    expect!(ctx.wait_for_next_slot_ephem(), validator);

    // Cancelling drains the crank back to the sponsor and refunds its rent, so
    // the sponsor ends up whole again apart from transaction fees.
    let sponsor_after =
        expect!(ctx.fetch_ephem_account_balance(&sponsor), validator);
    assert!(
        sponsor_after == sponsor_while_scheduled + min_funding,
        cleanup(&mut validator),
        "cancelling did not refund the crank budget: {sponsor_after} <= {sponsor_while_scheduled}"
    );
    assert!(
        sponsor_after == sponsor_before,
        cleanup(&mut validator),
        "sponsor was drained across a schedule/cancel cycle: {sponsor_after} vs {sponsor_before}"
    );

    cleanup(&mut validator);
}
