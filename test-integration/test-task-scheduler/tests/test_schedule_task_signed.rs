use integration_test_tools::{expect, validator::cleanup};
use magicblock_program::ID as MAGIC_PROGRAM_ID;
use program_flexi_counter::instruction::create_schedule_task_ix;
use solana_sdk::{
    instruction::InstructionError,
    native_token::LAMPORTS_PER_SOL,
    signature::Keypair,
    signer::Signer,
    transaction::{Transaction, TransactionError},
};
use test_task_scheduler::{
    create_delegated_counter, send_noop_tx, setup_validator,
};

/// Test that a task can be scheduled and executed when it has multiple signers
#[test]
fn test_schedule_task_signed() {
    let (_temp_dir, mut validator, ctx) = setup_validator();
    let payer = Keypair::new();

    expect!(
        ctx.airdrop_chain(&payer.pubkey(), 10 * LAMPORTS_PER_SOL),
        validator
    );

    create_delegated_counter(&ctx, &payer, &mut validator);

    // Noop tx to make sure the noop program is cloned
    let ephem_blockhash = send_noop_tx(&ctx, &payer, &mut validator);

    // Schedule a task
    let task_id = 4;
    let execution_interval_millis = 100;
    let iterations = 3;
    let sig = expect!(
        ctx.send_transaction_ephem(
            &mut Transaction::new_signed_with_payer(
                &[create_schedule_task_ix(
                    payer.pubkey(),
                    MAGIC_PROGRAM_ID,
                    task_id,
                    execution_interval_millis,
                    iterations,
                    false,
                    true,
                )],
                Some(&payer.pubkey()),
                &[&payer],
                ephem_blockhash,
            ),
            &[&payer],
        ),
        validator
    );
    expect!(ctx.wait_for_next_slot_ephem(), validator);
    let status = expect!(ctx.get_transaction_ephem(&sig), validator);
    expect!(
        status
            .transaction
            .meta
            .map(|m| matches!(
                m.err,
                Some(TransactionError::InstructionError(
                    0,
                    InstructionError::MissingRequiredSignature
                ))
            ))
            .ok_or_else(|| anyhow::anyhow!("Expected error not found")),
        validator
    );

    cleanup(&mut validator);
}
