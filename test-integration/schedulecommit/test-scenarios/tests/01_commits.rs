use integration_test_tools::{run_test, IntegrationTestContext};
use log::*;
use program_schedulecommit::{
    api::{
        delegate_account_cpi_instruction, init_account_instruction,
        pda_and_bump, schedule_commit_cpi_instruction,
    },
    ScheduleCommitCpiArgs, ScheduleCommitInstruction,
};
use schedulecommit_client::{verify, ScheduleCommitTestContextFields};
use solana_rpc_client::rpc_client::SerializableTransaction;
use solana_rpc_client_api::config::RpcSendTransactionConfig;
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL,
    pubkey::Pubkey,
    signature::{Keypair, Signature},
    signer::Signer,
    transaction::{Transaction, TransactionError},
};
use test_kit::{init_logger, AccountMeta, Instruction};
use utils::{
    assert_one_committee_synchronized_count,
    assert_one_committee_was_committed,
    assert_two_committees_synchronized_count,
    assert_two_committees_were_committed,
    get_context_with_delegated_committees,
};

use crate::utils::extract_transaction_error;

mod utils;

// NOTE: This and all other schedule commit tests depend on the following accounts
//       loaded in the mainnet cluster, i.e. the solana-test-validator:
//
//  validator:                tEsT3eV6RFCWs1BZ7AXTzasHqTtMnMLCB2tjQ42TDXD
//  protocol fees vault:      7JrkjmZPprHwtuvtuGTXp9hwfGYFAQLnLeFM52kqAgXg
//  validator fees vault:     DUH8h7rYjdTPYyBUEGAUwZv9ffz5wiM45GdYWYzogXjp
//  delegation program:       DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh
//  committor program:        ComtrB2KEaWgXsW1dhr1xYL4Ht4Bjj3gXnnL6KMdABq

#[test]
fn test_committing_one_account() {
    run_test!({
        let ctx = get_context_with_delegated_committees(1);

        let ScheduleCommitTestContextFields {
            payer_ephem: payer,
            committees,
            commitment,
            ephem_client,
            ..
        } = ctx.fields();

        debug!("Context initialized: {ctx}");

        let ix = schedule_commit_cpi_instruction(
            payer.pubkey(),
            magicblock_magic_program_api::id(),
            magicblock_magic_program_api::MAGIC_CONTEXT_PUBKEY,
            &committees
                .iter()
                .map(|(player, _)| player.pubkey())
                .collect::<Vec<_>>(),
            &committees.iter().map(|(_, pda)| *pda).collect::<Vec<_>>(),
        );

        let ephem_blockhash = ephem_client.get_latest_blockhash().unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&payer.pubkey()),
            &[&payer],
            ephem_blockhash,
        );

        let sig = tx.get_signature();
        debug!("Submitting tx to commit committee {sig}",);
        let res = ephem_client
            .send_and_confirm_transaction_with_spinner_and_config(
                &tx,
                *commitment,
                RpcSendTransactionConfig {
                    skip_preflight: true,
                    ..Default::default()
                },
            );
        info!("{} '{:?}'", sig, res);

        let res = verify::fetch_and_verify_commit_result_from_logs(&ctx, *sig);
        assert_one_committee_was_committed(&ctx, &res, true);
        assert_one_committee_synchronized_count(&ctx, &res, 1);
    });
}

#[test]
fn test_committing_two_accounts() {
    run_test!({
        let ctx = get_context_with_delegated_committees(2);

        let ScheduleCommitTestContextFields {
            payer_ephem: payer,
            committees,
            commitment,
            ephem_client,
            ..
        } = ctx.fields();

        let ix = schedule_commit_cpi_instruction(
            payer.pubkey(),
            magicblock_magic_program_api::id(),
            magicblock_magic_program_api::MAGIC_CONTEXT_PUBKEY,
            &committees
                .iter()
                .map(|(player, _)| player.pubkey())
                .collect::<Vec<_>>(),
            &committees.iter().map(|(_, pda)| *pda).collect::<Vec<_>>(),
        );

        let ephem_blockhash = ephem_client.get_latest_blockhash().unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&payer.pubkey()),
            &[&payer],
            ephem_blockhash,
        );

        let sig = tx.get_signature();
        let res = ephem_client
            .send_and_confirm_transaction_with_spinner_and_config(
                &tx,
                *commitment,
                RpcSendTransactionConfig {
                    skip_preflight: true,
                    ..Default::default()
                },
            );
        info!("{} '{:?}'", sig, res);

        let res = verify::fetch_and_verify_commit_result_from_logs(&ctx, *sig);
        assert_two_committees_were_committed(&ctx, &res, true);
        assert_two_committees_synchronized_count(&ctx, &res, 1);
    });
}

#[test]
fn test_committing_account_delegated_to_another_validator() {
    run_test!({
        let ctx = IntegrationTestContext::try_new().unwrap();

        // Init other validator
        let other_validator = Keypair::new();
        ctx.airdrop_chain(&other_validator.pubkey(), LAMPORTS_PER_SOL)
            .unwrap();

        // Init payer
        let payer = Keypair::new();
        ctx.airdrop_chain(&payer.pubkey(), LAMPORTS_PER_SOL)
            .unwrap();

        // Init + delegate player to other validator
        let (player, player_pda) = init_and_delegate_player(
            &ctx,
            &payer,
            Some(other_validator.pubkey()),
        );

        // Schedule commit of account delegated to another validator
        let res = schedule_commit_tx(&ctx, &payer, &player, player_pda, false);

        // We expect InvalidAccountForFee error since account isn't delegated to our validator
        let (_, tx_err) = extract_transaction_error(res);
        assert_eq!(tx_err.unwrap(), TransactionError::InvalidAccountForFee)
    });
}

#[test]
fn test_undelegating_account_delegated_to_another_validator() {
    run_test!({
        let ctx = IntegrationTestContext::try_new().unwrap();

        // Init other validator
        let other_validator = Keypair::new();
        ctx.airdrop_chain(&other_validator.pubkey(), LAMPORTS_PER_SOL)
            .unwrap();

        // Init payer
        let payer = Keypair::new();
        ctx.airdrop_chain(&payer.pubkey(), LAMPORTS_PER_SOL)
            .unwrap();

        // Init + delegate player to other validator
        let (player, player_pda) = init_and_delegate_player(
            &ctx,
            &payer,
            Some(other_validator.pubkey()),
        );

        // Schedule undelegation of account delegated to another validator
        let res = schedule_commit_tx(&ctx, &payer, &player, player_pda, true);

        // We expect InvalidWriteableAccount error since account isn't delegated to our validator
        let (_, tx_err) = extract_transaction_error(res);
        assert_eq!(tx_err.unwrap(), TransactionError::InvalidWritableAccount);
    });
}

fn init_and_delegate_player(
    ctx: &IntegrationTestContext,
    payer: &Keypair,
    validator: Option<Pubkey>,
) -> (Keypair, Pubkey) {
    // Create player and derive its PDA
    let player = Keypair::new();
    let (player_pda, _) = pda_and_bump(&player.pubkey());

    // Build init + delegate instructions
    let init_ix =
        init_account_instruction(payer.pubkey(), player.pubkey(), player_pda);
    let delegate_ix = delegate_account_cpi_instruction(
        payer.pubkey(),
        validator,
        player.pubkey(),
    );

    // Send transaction
    let mut tx = Transaction::new_signed_with_payer(
        &[init_ix, delegate_ix],
        Some(&payer.pubkey()),
        &[payer, &player],
        Default::default(),
    );
    let signature = ctx
        .send_transaction_chain(&mut tx, &[payer, &player])
        .unwrap();
    debug!("init+delegate player tx signature: {}", signature);

    (player, player_pda)
}

fn schedule_commit_tx(
    ctx: &IntegrationTestContext,
    payer: &Keypair,
    player: &Keypair,
    player_pda: Pubkey,
    is_undelegate: bool,
) -> solana_rpc_client_api::client_error::Result<Signature> {
    // Build the instruction
    let ephem_client = ctx.ephem_client.as_ref().unwrap();
    let ix = schedule_commit_cpi_illegal_owner(
        payer.pubkey(),
        magicblock_magic_program_api::id(),
        magicblock_magic_program_api::MAGIC_CONTEXT_PUBKEY,
        &[player.pubkey()],
        &[player_pda],
        is_undelegate,
    );

    // Build and send transaction
    let blockhash = ephem_client.get_latest_blockhash().unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&payer.pubkey()),
        &[payer],
        blockhash,
    );

    let res = ephem_client
        .send_and_confirm_transaction_with_spinner_and_config(
            &tx,
            ephem_client.commitment(),
            RpcSendTransactionConfig {
                skip_preflight: true,
                ..Default::default()
            },
        );
    debug!("schedule commit tx signature: {}", tx.get_signature());

    res
}

fn schedule_commit_cpi_illegal_owner(
    payer: Pubkey,
    magic_program_id: Pubkey,
    magic_context_id: Pubkey,
    players: &[Pubkey],
    committees: &[Pubkey],
    is_undelegate: bool,
) -> Instruction {
    let program_id = program_schedulecommit::id();
    let mut account_metas = vec![
        AccountMeta::new(payer, true),
        AccountMeta::new(magic_context_id, false),
        AccountMeta::new_readonly(magic_program_id, false),
    ];
    for committee in committees {
        account_metas.push(if is_undelegate {
            AccountMeta::new(*committee, false)
        } else {
            AccountMeta::new_readonly(*committee, false)
        });
    }

    let cpi_args = ScheduleCommitCpiArgs {
        players: players.to_vec(),
        modify_accounts: false,
        undelegate: is_undelegate,
        commit_payer: true,
    };
    let ix = ScheduleCommitInstruction::ScheduleCommitCpi(cpi_args);
    Instruction::new_with_borsh(program_id, &ix, account_metas)
}
