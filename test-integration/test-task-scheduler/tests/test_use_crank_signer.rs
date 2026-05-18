use std::time::Duration;

use integration_test_tools::{expect, validator::cleanup};
use magicblock_program::{
    args::ScheduleTaskArgs, instruction_utils::InstructionUtils,
    pda::CRANK_SIGNER,
};
use program_flexi_counter::{
    instruction::FlexiCounterInstruction, state::FlexiCounter,
};
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    native_token::LAMPORTS_PER_SOL,
    signature::Keypair,
    signer::Signer,
    transaction::Transaction,
};
use test_task_scheduler::{
    create_delegated_counter, setup_validator, wait_for_incremented_counter,
};

#[test]
fn test_use_crank_signer() {
    let (_temp_dir, mut validator, ctx) = setup_validator();

    let payer = Keypair::new();
    let (counter_pda, _) = FlexiCounter::pda(&payer.pubkey());

    expect!(
        ctx.airdrop_chain(&payer.pubkey(), 10 * LAMPORTS_PER_SOL),
        validator
    );

    create_delegated_counter(&ctx, &payer, &mut validator, 0);

    let ephem_blockhash =
        expect!(ctx.try_get_latest_blockhash_ephem(), validator);

    // Schedule a task
    let task_id = 9;
    let execution_interval_millis = 10;
    let iterations = 5;
    let sig = expect!(
        ctx.send_transaction_ephem_with_preflight(
            &mut Transaction::new_signed_with_payer(
                &[InstructionUtils::schedule_task_instruction(
                    &payer.pubkey(),
                    ScheduleTaskArgs {
                        task_id,
                        execution_interval_millis,
                        iterations,
                        instructions: vec![Instruction::new_with_borsh(
                            program_flexi_counter::ID,
                            &FlexiCounterInstruction::AddUnsigned { count: 1 },
                            vec![
                                AccountMeta::new(counter_pda, false),
                                AccountMeta::new_readonly(CRANK_SIGNER, true)
                            ],
                        )],
                    }
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

    // Check that the counter was incremented
    wait_for_incremented_counter(
        &ctx,
        &counter_pda,
        iterations as u64,
        Duration::from_secs(10),
        &mut validator,
    );

    cleanup(&mut validator);
}
