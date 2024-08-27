use std::str::FromStr;

use schedulecommit_client::{verify, ScheduleCommitTestContext};
use schedulecommit_program::api::{
    increase_count_instruction, schedule_commit_and_undelegate_cpi_instruction,
};
use sleipnir_core::magic_program;
use solana_rpc_client::rpc_client::SerializableTransaction;
use solana_rpc_client_api::config::RpcSendTransactionConfig;
use solana_sdk::{
    instruction::InstructionError, pubkey::Pubkey, signature::Signature,
    signer::Signer, transaction::Transaction,
};
use utils::{
    assert_one_committee_account_was_locked_on_ephem,
    assert_one_committee_account_was_undelegated_on_chain,
    assert_one_committee_synchronized_count,
    assert_one_committee_was_committed,
    assert_two_committee_accounts_were_locked_on_ephem,
    assert_two_committee_accounts_were_undelegated_on_chain,
    assert_two_committees_synchronized_count,
    assert_two_committees_were_committed,
    assert_tx_failed_with_instruction_error,
    get_context_with_delegated_committees,
};

mod utils;

fn commit_and_undelegate_one_account() -> (ScheduleCommitTestContext, Signature)
{
    let ctx = get_context_with_delegated_committees(1);
    let ScheduleCommitTestContext {
        payer,
        committees,
        commitment,
        ephem_client,
        ephem_blockhash,
        ..
    } = &ctx;

    let ix = schedule_commit_and_undelegate_cpi_instruction(
        payer.pubkey(),
        // Work around the different solana_sdk versions by creating pubkey from str
        Pubkey::from_str(magic_program::MAGIC_PROGRAM_ADDR).unwrap(),
        &committees
            .iter()
            .map(|(player, _)| player.pubkey())
            .collect::<Vec<_>>(),
        &committees.iter().map(|(_, pda)| *pda).collect::<Vec<_>>(),
    );

    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&payer.pubkey()),
        &[&payer],
        *ephem_blockhash,
    );

    let sig = tx.get_signature();
    let tx_res = ephem_client
        .send_and_confirm_transaction_with_spinner_and_config(
            &tx,
            *commitment,
            RpcSendTransactionConfig {
                skip_preflight: true,
                ..Default::default()
            },
        );
    eprintln!("Commit and Undelegate Transaction result: '{:?}'", tx_res);
    (ctx, *sig)
}

fn commit_and_undelegate_two_accounts() -> (ScheduleCommitTestContext, Signature)
{
    let ctx = get_context_with_delegated_committees(2);
    let ScheduleCommitTestContext {
        payer,
        committees,
        commitment,
        ephem_client,
        ephem_blockhash,
        ..
    } = &ctx;

    let ix = schedule_commit_and_undelegate_cpi_instruction(
        payer.pubkey(),
        // Work around the different solana_sdk versions by creating pubkey from str
        Pubkey::from_str(magic_program::MAGIC_PROGRAM_ADDR).unwrap(),
        &committees
            .iter()
            .map(|(player, _)| player.pubkey())
            .collect::<Vec<_>>(),
        &committees.iter().map(|(_, pda)| *pda).collect::<Vec<_>>(),
    );

    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&payer.pubkey()),
        &[&payer],
        *ephem_blockhash,
    );

    let sig = tx.get_signature();
    let tx_res = ephem_client
        .send_and_confirm_transaction_with_spinner_and_config(
            &tx,
            *commitment,
            RpcSendTransactionConfig {
                skip_preflight: true,
                ..Default::default()
            },
        );
    eprintln!("Commit and Undelegate Transaction result: '{:?}'", tx_res);
    (ctx, *sig)
}

#[test]
fn test_committing_and_undelegating_one_account() {
    let (ctx, sig) = commit_and_undelegate_one_account();

    let res = verify::fetch_commit_result_from_logs(&ctx, sig);

    assert_one_committee_was_committed(&ctx, &res);
    assert_one_committee_synchronized_count(&ctx, &res, 1);

    assert_one_committee_account_was_undelegated_on_chain(&ctx);
    assert_one_committee_account_was_locked_on_ephem(&ctx, &res);
}

#[test]
fn test_committing_and_undelegating_two_accounts() {
    let (ctx, sig) = commit_and_undelegate_two_accounts();

    let res = verify::fetch_commit_result_from_logs(&ctx, sig);

    assert_two_committees_were_committed(&ctx, &res);
    assert_two_committees_synchronized_count(&ctx, &res, 1);

    assert_two_committee_accounts_were_undelegated_on_chain(&ctx);
    assert_two_committee_accounts_were_locked_on_ephem(&ctx);
}

#[test]
fn test_committed_and_undelegated_single_account_usage() {
    let (ctx, sig) = commit_and_undelegate_one_account();
    let ScheduleCommitTestContext {
        payer,
        committees,
        commitment,
        ephem_client,
        ephem_blockhash,
        chain_client,
        chain_blockhash,
        ..
    } = &ctx;

    // 1. Show we cannot use it in the ehpemeral anymore
    // TODO: function
    {
        let pda1 = committees[0].1;
        let ix = increase_count_instruction(pda1);
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&payer.pubkey()),
            &[&payer],
            *ephem_blockhash,
        );
        let tx_res = ephem_client
            .send_and_confirm_transaction_with_spinner_and_config(
                &tx,
                *commitment,
                RpcSendTransactionConfig {
                    skip_preflight: true,
                    ..Default::default()
                },
            );
        assert_tx_failed_with_instruction_error(
            tx_res,
            InstructionError::ExternalAccountDataModified,
        );
    }

    // 2. Show that we cannot use it on chain while it is being undelegated
    // TODO: function
    {
        let pda1 = committees[0].1;
        let ix = increase_count_instruction(pda1);
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&payer.pubkey()),
            &[&payer],
            *chain_blockhash,
        );
        let tx_res = chain_client
            .send_and_confirm_transaction_with_spinner_and_config(
                &tx,
                *commitment,
                RpcSendTransactionConfig {
                    skip_preflight: true,
                    ..Default::default()
                },
            );
        assert_tx_failed_with_instruction_error(
            tx_res,
            InstructionError::ExternalAccountDataModified,
        );
    }

    // 3. Wait for commit + undelegation to finish and try chain again
    {
        verify::fetch_commit_result_from_logs(&ctx, sig);

        // we need a new blockhash otherwise the tx is identical to the above
        let blockhash = chain_client.get_latest_blockhash().unwrap();

        let pda1 = committees[0].1;
        let ix = increase_count_instruction(pda1);
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&payer.pubkey()),
            &[&payer],
            blockhash,
        );
        let tx_res = chain_client
            .send_and_confirm_transaction_with_spinner_and_config(
                &tx,
                *commitment,
                RpcSendTransactionConfig {
                    skip_preflight: true,
                    ..Default::default()
                },
            );

        eprintln!("Increase Count Transaction result: '{:?}'", tx_res);
        assert!(tx_res.is_ok());
    }

    // 4. Now try using the undelegated account again on ephem, it should still fail,
    //    but the error should indicate that the account is not delegated
    {
        // we need a new blockhash otherwise the tx is identical to the above
        let blockhash = ephem_client.get_latest_blockhash().unwrap();

        let pda1 = committees[0].1;
        let ix = increase_count_instruction(pda1);
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&payer.pubkey()),
            &[&payer],
            blockhash,
        );
        let tx_res = ephem_client
            .send_and_confirm_transaction_with_spinner_and_config(
                &tx,
                *commitment,
                RpcSendTransactionConfig {
                    skip_preflight: true,
                    ..Default::default()
                },
            );
        // TODO: @@@
        eprintln!("Increase Count Transaction result: '{:?}'", tx_res);
    }
}

#[test]
fn test_committed_and_undelegated_accounts_usage() {
    let (ctx, sig) = commit_and_undelegate_two_accounts();
    let ScheduleCommitTestContext {
        payer,
        committees,
        commitment,
        ephem_client,
        ephem_blockhash,
        chain_client,
        chain_blockhash,
        ..
    } = &ctx;

    // 1. Show we cannot use it in the ehpemeral anymore
    {
        let pda1 = committees[0].1;
        let ix = increase_count_instruction(pda1);
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&payer.pubkey()),
            &[&payer],
            *ephem_blockhash,
        );
        let tx_res = ephem_client
            .send_and_confirm_transaction_with_spinner_and_config(
                &tx,
                *commitment,
                RpcSendTransactionConfig {
                    skip_preflight: true,
                    ..Default::default()
                },
            );
        assert_tx_failed_with_instruction_error(
            tx_res,
            InstructionError::ExternalAccountDataModified,
        );
    }

    // 2. Show that we cannot use it on chain while it is being undelegated
    {
        let pda1 = committees[0].1;
        let ix = increase_count_instruction(pda1);
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&payer.pubkey()),
            &[&payer],
            *chain_blockhash,
        );
        let tx_res = chain_client
            .send_and_confirm_transaction_with_spinner_and_config(
                &tx,
                *commitment,
                RpcSendTransactionConfig {
                    skip_preflight: true,
                    ..Default::default()
                },
            );
        assert_tx_failed_with_instruction_error(
            tx_res,
            InstructionError::ExternalAccountDataModified,
        );
    }

    // 3. Wait for commit + undelegation to finish and try chain again
    {
        verify::fetch_commit_result_from_logs(&ctx, sig);

        // we need a new blockhash otherwise the tx is identical to the above
        let blockhash = chain_client.get_latest_blockhash().unwrap();

        let pda1 = committees[0].1;
        let ix = increase_count_instruction(pda1);
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&payer.pubkey()),
            &[&payer],
            blockhash,
        );
        let tx_res = chain_client
            .send_and_confirm_transaction_with_spinner_and_config(
                &tx,
                *commitment,
                RpcSendTransactionConfig {
                    skip_preflight: true,
                    ..Default::default()
                },
            );

        eprintln!("Increase Count Transaction result: '{:?}'", tx_res);
        assert!(tx_res.is_ok());
    }

    // 4. Now try using the undelegated account again on ephem, it should still fail,
    //    but the error should indicate that the account is not delegated
    {
        // we need a new blockhash otherwise the tx is identical to the above
        let blockhash = ephem_client.get_latest_blockhash().unwrap();

        let pda1 = committees[0].1;
        let ix = increase_count_instruction(pda1);
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&payer.pubkey()),
            &[&payer],
            blockhash,
        );
        let tx_res = ephem_client
            .send_and_confirm_transaction_with_spinner_and_config(
                &tx,
                *commitment,
                RpcSendTransactionConfig {
                    skip_preflight: true,
                    ..Default::default()
                },
            );
        // TODO: @@@
        eprintln!("Increase Count Transaction result: '{:?}'", tx_res);
    }
}
