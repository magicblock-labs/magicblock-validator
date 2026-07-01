use std::time::Duration;

use hydra_api::instruction::ephemeral;
use integration_test_tools::{expect, validator::cleanup};
use magicblock_task_scheduler::crank_pubkey;
use program_flexi_counter::{
    instruction::{create_schedule_task_ix, FlexiCounterInstruction},
    state::FlexiCounter,
};
use solana_sdk::{
    message::{AccountMeta, Instruction},
    native_token::LAMPORTS_PER_SOL,
    signature::Keypair,
    signer::Signer,
    transaction::Transaction,
};
use test_task_scheduler::{
    create_delegated_counter, setup_validator, wait_for_hydra_crank,
};

/// The validator identity must not be drained to pay for hydra cranks: a
/// dedicated, delegated faucet account pays instead. Scheduling a task creates
/// and funds a crank, and the validator identity's balance is left untouched.
#[test]
fn test_undrained_faucet() {
    let (_temp_dir, mut validator, ctx, faucet_keypair) = setup_validator();

    let payer = Keypair::new();
    let (counter_pda, _) = FlexiCounter::pda(&payer.pubkey());

    expect!(
        ctx.airdrop_chain(&payer.pubkey(), 10 * LAMPORTS_PER_SOL),
        validator
    );

    create_delegated_counter(&ctx, &payer, &mut validator, 0);

    // The default validator identity keypair (see consts::DEFAULT_VALIDATOR_KEYPAIR).
    let faucet_keypair = expect!(
        faucet_keypair
            .ok_or_else(|| anyhow::anyhow!("Faucet keypair not found")),
        validator
    );
    let faucet_pk = faucet_keypair.pubkey();
    let faucet_balance_before =
        expect!(ctx.fetch_ephem_account_balance(&faucet_pk), validator);

    let mut ephem_blockhash =
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

    // The faucet executes the crank
    for _ in 0..iterations {
        ephem_blockhash =
            expect!(ctx.try_get_latest_blockhash_ephem(), validator);
        let sig = expect!(
            ctx.send_transaction_ephem_with_preflight(
                &mut Transaction::new_signed_with_payer(
                    &[
                        ephemeral::trigger(crank_pda, faucet_pk,),
                        Instruction::new_with_borsh(
                            program_flexi_counter::ID,
                            &FlexiCounterInstruction::AddUnsigned { count: 1 },
                            vec![AccountMeta::new(counter_pda, false)],
                        )
                    ],
                    Some(&faucet_pk),
                    &[&faucet_keypair],
                    ephem_blockhash,
                ),
                &[&faucet_keypair]
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
        expect!(ctx.wait_for_next_slot_ephem(), validator);
    }

    // Close the crank
    let sig = expect!(
        ctx.send_transaction_ephem_with_preflight(
            &mut Transaction::new_signed_with_payer(
                &[ephemeral::close(faucet_pk, crank_pda, faucet_pk)],
                Some(&faucet_pk),
                &[&faucet_keypair],
                ephem_blockhash,
            ),
            &[&faucet_keypair]
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

    // The validator identity must not have been drained to pay for the crank.
    let faucet_balance_after =
        expect!(ctx.fetch_ephem_account_balance(&faucet_pk), validator);
    assert!(
        faucet_balance_after >= faucet_balance_before,
        "faucet was drained paying for cranks: {} < {}",
        faucet_balance_after,
        faucet_balance_before
    );

    cleanup(&mut validator);
}
