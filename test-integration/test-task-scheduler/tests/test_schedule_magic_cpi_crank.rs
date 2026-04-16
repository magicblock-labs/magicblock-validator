use std::time::Duration;

use integration_test_tools::{expect, validator::cleanup};
use magicblock_program::{
    args::ScheduleTaskArgs, instruction_utils::InstructionUtils,
    pda::CRANK_SIGNER, MAGIC_CONTEXT_PUBKEY,
};
use program_schedulecommit::{
    api::{
        delegate_account_cpi_instruction, init_account_instruction,
        schedule_commit_cpi_instruction, UserSeeds,
    },
    ScheduleCommitType,
};
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, pubkey::Pubkey, signature::Keypair,
    signer::Signer, transaction::Transaction,
};
use test_task_scheduler::{setup_validator, wait_for_committed_count};

#[test]
fn test_crank_can_execute_program_that_cpis_into_magic() {
    let (_temp_dir, mut validator, ctx) = setup_validator();

    let player = Keypair::new();
    let (committee, _) = Pubkey::find_program_address(
        &[
            UserSeeds::MagicScheduleCommit.bytes(),
            player.pubkey().as_ref(),
        ],
        &program_schedulecommit::ID,
    );

    expect!(
        ctx.airdrop_chain(&player.pubkey(), 10 * LAMPORTS_PER_SOL),
        validator
    );

    let chain_blockhash = expect!(
        ctx.try_chain_client().and_then(|client| client
            .get_latest_blockhash()
            .map_err(|e| anyhow::anyhow!(
                "Failed to get latest chain blockhash: {}",
                e
            ))),
        validator
    );

    expect!(
        ctx.send_transaction_chain(
            &mut Transaction::new_signed_with_payer(
                &[init_account_instruction(
                    player.pubkey(),
                    player.pubkey(),
                    committee,
                )],
                Some(&player.pubkey()),
                &[&player],
                chain_blockhash,
            ),
            &[&player]
        ),
        validator
    );

    expect!(
        ctx.send_transaction_chain(
            &mut Transaction::new_signed_with_payer(
                &[delegate_account_cpi_instruction(
                    player.pubkey(),
                    None,
                    player.pubkey(),
                    UserSeeds::MagicScheduleCommit,
                )],
                Some(&player.pubkey()),
                &[&player],
                chain_blockhash,
            ),
            &[&player]
        ),
        validator
    );

    expect!(ctx.wait_for_delta_slot_ephem(10), validator);

    let ephem_blockhash =
        expect!(ctx.try_get_latest_blockhash_ephem(), validator);
    let crank_ix = schedule_commit_cpi_instruction(
        CRANK_SIGNER,
        magicblock_program::ID,
        MAGIC_CONTEXT_PUBKEY,
        None,
        &[player.pubkey()],
        &[committee],
        ScheduleCommitType::Commit,
    );

    let schedule_sig = expect!(
        ctx.send_transaction_ephem_with_preflight(
            &mut Transaction::new_signed_with_payer(
                &[InstructionUtils::schedule_task_instruction(
                    &player.pubkey(),
                    ScheduleTaskArgs {
                        task_id: 17,
                        execution_interval_millis: 10,
                        iterations: 1,
                        instructions: vec![crank_ix],
                    },
                )],
                Some(&player.pubkey()),
                &[&player],
                ephem_blockhash,
            ),
            &[&player]
        ),
        validator
    );
    let schedule_status =
        expect!(ctx.get_transaction_ephem(&schedule_sig), validator);
    expect!(
        schedule_status
            .transaction
            .meta
            .and_then(|m| m.status.ok())
            .ok_or_else(|| anyhow::anyhow!("Scheduling transaction failed")),
        validator
    );

    wait_for_committed_count(
        &ctx,
        &committee,
        1,
        Duration::from_secs(15),
        &mut validator,
    );

    cleanup(&mut validator);
}
