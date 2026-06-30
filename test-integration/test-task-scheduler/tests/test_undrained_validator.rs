use std::time::Duration;

use integration_test_tools::{expect, validator::cleanup};
use magicblock_task_scheduler::crank_pubkey;
use program_flexi_counter::{
    instruction::create_schedule_task_ix, state::FlexiCounter,
};
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, signature::Keypair, signer::Signer,
    transaction::Transaction,
};
use test_task_scheduler::{
    create_delegated_counter, setup_validator, wait_for_hydra_crank,
};

/// The validator identity must not be drained to pay for hydra cranks: a
/// dedicated, delegated faucet account pays instead. Scheduling a task creates
/// and funds a crank, and the validator identity's balance is left untouched.
#[test]
fn test_undrained_validator() {
    let (_temp_dir, mut validator, ctx) = setup_validator();

    let payer = Keypair::new();
    let (_counter_pda, _) = FlexiCounter::pda(&payer.pubkey());

    expect!(
        ctx.airdrop_chain(&payer.pubkey(), 10 * LAMPORTS_PER_SOL),
        validator
    );

    create_delegated_counter(&ctx, &payer, &mut validator, 0);

    // The default validator identity keypair (see consts::DEFAULT_VALIDATOR_KEYPAIR).
    let validator_keypair = Keypair::from_base58_string("9Vo7TbA5YfC5a33JhAi9Fb41usA6JwecHNRw3f9MzzHAM8hFnXTzL5DcEHwsAFjuUZ8vNQcJ4XziRFpMc3gTgBQ");
    let validator_identity = validator_keypair.pubkey();
    let validator_balance_before = expect!(
        ctx.fetch_ephem_account_balance(&validator_identity),
        validator
    );

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

    // The crank is created by hydra and funded for every iteration (paid for by
    // the faucet, not the validator identity).
    let crank_pda = crank_pubkey(&payer.pubkey(), task_id);
    wait_for_hydra_crank(
        &ctx,
        &crank_pda,
        Duration::from_secs(10),
        &mut validator,
    );

    // The validator identity must not have been drained to pay for the crank.
    let validator_balance_after = expect!(
        ctx.fetch_ephem_account_balance(&validator_identity),
        validator
    );
    assert!(
        validator_balance_after >= validator_balance_before,
        "validator identity was drained paying for cranks: {} < {}",
        validator_balance_after,
        validator_balance_before
    );

    cleanup(&mut validator);
}
