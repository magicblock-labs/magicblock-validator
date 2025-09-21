use cleanass::assert;
use integration_test_tools::{expect, validator::cleanup};
use magicblock_program::{ID as MAGIC_PROGRAM_ID, TASK_CONTEXT_PUBKEY};
use program_flexi_counter::instruction::{
    create_delegate_ix, create_init_ix, create_schedule_task_ix,
};
use solana_sdk::{
    instruction::InstructionError,
    native_token::LAMPORTS_PER_SOL,
    signature::Keypair,
    signer::Signer,
    transaction::{Transaction, TransactionError},
};
use test_task_scheduler::{send_memo_tx, setup_validator};

/// Test that a task can be scheduled and executed when it has multiple signers
#[test]
fn test_schedule_task_signed() {
    let (_temp_dir, mut validator, ctx) = setup_validator();
    let payer = Keypair::new();

    expect!(
        ctx.airdrop_chain(&payer.pubkey(), 10 * LAMPORTS_PER_SOL),
        validator
    );

    // Initialize the counter
    let blockhash = expect!(
        ctx.try_chain_client().and_then(|client| client
            .get_latest_blockhash()
            .map_err(|e| anyhow::anyhow!(
                "Failed to get latest blockhash: {}",
                e
            ))),
        validator
    );
    expect!(
        ctx.send_transaction_chain(
            &mut Transaction::new_signed_with_payer(
                &[create_init_ix(payer.pubkey(), "test".to_string())],
                Some(&payer.pubkey()),
                &[&payer],
                blockhash,
            ),
            &[&payer]
        ),
        validator
    );

    // Delegate the counter to the ephem validator
    expect!(
        ctx.send_transaction_chain(
            &mut Transaction::new_signed_with_payer(
                &[create_delegate_ix(payer.pubkey())],
                Some(&payer.pubkey()),
                &[&payer],
                blockhash,
            ),
            &[&payer]
        ),
        validator
    );

    // Wait for account to be delegated
    expect!(ctx.wait_for_delta_slot_ephem(10), validator);

    // Noop tx to make sure the noop program is cloned
    let ephem_blockhash = send_memo_tx(&ctx, &payer, &mut validator);

    // Schedule a task
    let task_id = 4;
    let execution_interval_millis = 100;
    let iterations = 3;
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
            .ok_or_else(|| anyhow::anyhow!("No meta in transaction")),
        validator
    );

    cleanup(&mut validator);
}
