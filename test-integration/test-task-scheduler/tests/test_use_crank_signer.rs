use cleanass::assert;
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
use test_task_scheduler::{create_delegated_counter, setup_validator};

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

    // Wait for the task to be scheduled
    expect!(ctx.wait_for_delta_slot_ephem(10), validator);

    // Check that the counter was incremented
    let counter_account = expect!(
        ctx.try_ephem_client().and_then(|client| client
            .get_account(&counter_pda)
            .map_err(|e| anyhow::anyhow!("Failed to get account: {}", e))),
        validator
    );
    let counter =
        expect!(FlexiCounter::try_decode(&counter_account.data), validator);
    assert!(
        counter.count == iterations as u64,
        cleanup(&mut validator),
        "counter.count: {}",
        counter.count
    );

    cleanup(&mut validator);
}
