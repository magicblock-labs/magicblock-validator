use std::time::Duration;

use hydra_api::CRANKER_REWARD;
use integration_test_tools::{expect, validator::cleanup};
use magicblock_task_scheduler::crank_pubkey;
use program_flexi_counter::instruction::{
    create_cancel_task_ix, create_schedule_task_ix,
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
fn test_independent_cranks_per_authority() {
    let (_temp_dir, mut validator, ctx) = setup_validator();

    let payer = Keypair::new();
    let other = Keypair::new();

    expect!(
        ctx.airdrop_chain(&payer.pubkey(), 10 * LAMPORTS_PER_SOL),
        validator
    );
    expect!(
        ctx.airdrop_chain(&other.pubkey(), 10 * LAMPORTS_PER_SOL),
        validator
    );

    create_delegated_counter(&ctx, &payer, &mut validator, 0);
    create_delegated_counter(&ctx, &other, &mut validator, 0);

    let ephem_blockhash =
        expect!(ctx.try_get_latest_blockhash_ephem(), validator);

    // Both authorities schedule the same task_id with different iteration counts.
    let task_id = 1;
    let payer_iterations = 3;
    let other_iterations = 6;
    for (signer, iterations) in
        [(&payer, payer_iterations), (&other, other_iterations)]
    {
        let sig = expect!(
            ctx.send_transaction_ephem_with_preflight(
                &mut Transaction::new_signed_with_payer(
                    &[create_schedule_task_ix(
                        signer.pubkey(),
                        task_id,
                        100,
                        iterations,
                        false,
                        false,
                    )],
                    Some(&signer.pubkey()),
                    &[signer],
                    ephem_blockhash,
                ),
                &[signer]
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
    }

    // Each authority owns its own independent crank at a distinct PDA, funded
    // for its own iteration count.
    let payer_crank = crank_pubkey(&payer.pubkey(), task_id);
    let other_crank = crank_pubkey(&other.pubkey(), task_id);
    assert_ne!(payer_crank, other_crank);
    wait_for_hydra_crank(
        &ctx,
        &payer_crank,
        payer_iterations as u64 * CRANKER_REWARD,
        Duration::from_secs(10),
        &mut validator,
    );
    wait_for_hydra_crank(
        &ctx,
        &other_crank,
        other_iterations as u64 * CRANKER_REWARD,
        Duration::from_secs(10),
        &mut validator,
    );

    // Cancelling one authority's task closes only its crank; the other remains.
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

    wait_for_hydra_crank_closed(
        &ctx,
        &payer_crank,
        Duration::from_secs(10),
        &mut validator,
    );
    // The other authority's crank is unaffected.
    wait_for_hydra_crank(
        &ctx,
        &other_crank,
        other_iterations as u64 * CRANKER_REWARD,
        Duration::from_secs(10),
        &mut validator,
    );

    cleanup(&mut validator);
}
