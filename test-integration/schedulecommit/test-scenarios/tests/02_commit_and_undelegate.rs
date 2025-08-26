use integration_test_tools::run_test;
use integration_test_tools::scheduled_commits::extract_scheduled_commit_sent_signature_from_logs;
use log::*;
use magicblock_core::magic_program;
use program_schedulecommit::api::{
    increase_count_instruction, schedule_commit_and_undelegate_cpi_instruction,
    schedule_commit_and_undelegate_cpi_with_mod_after_instruction,
};
use schedulecommit_client::{
    verify, ScheduleCommitTestContext, ScheduleCommitTestContextFields,
};
use solana_rpc_client::rpc_client::{RpcClient, SerializableTransaction};
use solana_rpc_client_api::{
    client_error::{Error as ClientError, ErrorKind},
    config::RpcSendTransactionConfig,
    request::RpcError,
};
use solana_sdk::{
    commitment_config::CommitmentConfig,
    hash::Hash,
    instruction::InstructionError,
    pubkey::Pubkey,
    signature::{Keypair, Signature},
    signer::Signer,
    transaction::Transaction,
};
use test_tools_core::init_logger;
use utils::{
    assert_is_instruction_error,
    assert_one_committee_account_was_undelegated_on_chain,
    assert_one_committee_synchronized_count,
    assert_one_committee_was_committed,
    assert_two_committee_accounts_were_undelegated_on_chain,
    assert_two_committees_synchronized_count,
    assert_two_committees_were_committed, extract_transaction_error,
    get_context_with_delegated_committees,
};

mod utils;

fn commit_and_undelegate_one_account(
    modify_after: bool,
) -> (
    ScheduleCommitTestContext,
    Signature,
    Result<Signature, ClientError>,
) {
    let ctx = get_context_with_delegated_committees(1);
    let ScheduleCommitTestContextFields {
        payer,
        committees,
        commitment,
        ephem_client,
        ephem_blockhash,
        ..
    } = ctx.fields();

    let ix = if modify_after {
        schedule_commit_and_undelegate_cpi_with_mod_after_instruction(
            payer.pubkey(),
            magic_program::id(),
            magic_program::MAGIC_CONTEXT_PUBKEY,
            &committees
                .iter()
                .map(|(player, _)| player.pubkey())
                .collect::<Vec<_>>(),
            &committees.iter().map(|(_, pda)| *pda).collect::<Vec<_>>(),
        )
    } else {
        schedule_commit_and_undelegate_cpi_instruction(
            payer.pubkey(),
            magic_program::id(),
            magic_program::MAGIC_CONTEXT_PUBKEY,
            &committees
                .iter()
                .map(|(player, _)| player.pubkey())
                .collect::<Vec<_>>(),
            &committees.iter().map(|(_, pda)| *pda).collect::<Vec<_>>(),
        )
    };
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
    debug!("Commit and Undelegate Transaction result: '{:?}'", tx_res);
    (ctx, *sig, tx_res)
}

fn commit_and_undelegate_two_accounts(
    modify_after: bool,
) -> (
    ScheduleCommitTestContext,
    Signature,
    Result<Signature, ClientError>,
) {
    let ctx = get_context_with_delegated_committees(2);
    let ScheduleCommitTestContextFields {
        payer,
        committees,
        commitment,
        ephem_client,
        ephem_blockhash,
        ..
    } = ctx.fields();

    let ix = if modify_after {
        schedule_commit_and_undelegate_cpi_with_mod_after_instruction(
            payer.pubkey(),
            magic_program::id(),
            magic_program::MAGIC_CONTEXT_PUBKEY,
            &committees
                .iter()
                .map(|(player, _)| player.pubkey())
                .collect::<Vec<_>>(),
            &committees.iter().map(|(_, pda)| *pda).collect::<Vec<_>>(),
        )
    } else {
        schedule_commit_and_undelegate_cpi_instruction(
            payer.pubkey(),
            magic_program::id(),
            magic_program::MAGIC_CONTEXT_PUBKEY,
            &committees
                .iter()
                .map(|(player, _)| player.pubkey())
                .collect::<Vec<_>>(),
            &committees.iter().map(|(_, pda)| *pda).collect::<Vec<_>>(),
        )
    };

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
    debug!("Commit and Undelegate Transaction result: '{:?}'", tx_res);
    (ctx, *sig, tx_res)
}

#[test]
fn test_committing_and_undelegating_one_account() {
    run_test!({
        let (ctx, sig, tx_res) = commit_and_undelegate_one_account(false);
        info!("'{}' {:?}", sig, tx_res);

        let res = verify::fetch_and_verify_commit_result_from_logs(&ctx, sig);

        assert_one_committee_was_committed(&ctx, &res, true);
        assert_one_committee_synchronized_count(&ctx, &res, 1);

        assert_one_committee_account_was_undelegated_on_chain(&ctx);
    });
}

#[test]
fn test_committing_and_undelegating_two_accounts_success() {
    run_test!({
        let (ctx, sig, tx_res) = commit_and_undelegate_two_accounts(false);
        info!("'{}' {:?}", sig, tx_res);

        let res = verify::fetch_and_verify_commit_result_from_logs(&ctx, sig);

        assert_two_committees_were_committed(&ctx, &res, true);
        assert_two_committees_synchronized_count(&ctx, &res, 1);

        assert_two_committee_accounts_were_undelegated_on_chain(&ctx);
    });
}

// -----------------
// Delegate -> Increase in Ephem -> Undelegate -> Increase in Chain
// -> Redelegate -> Increase in Ephem
// -----------------
fn assert_cannot_increase_committee_count(
    pda: Pubkey,
    payer: &Keypair,
    blockhash: Hash,
    client: &RpcClient,
    commitment: &CommitmentConfig,
) {
    let ix = increase_count_instruction(pda);
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&payer.pubkey()),
        &[&payer],
        blockhash,
    );
    let tx_res = client.send_and_confirm_transaction_with_spinner_and_config(
        &tx,
        *commitment,
        RpcSendTransactionConfig {
            skip_preflight: true,
            ..Default::default()
        },
    );
    let (tx_result_err, tx_err) = extract_transaction_error(tx_res);
    if let Some(tx_err) = tx_err {
        assert_is_instruction_error(
            tx_err,
            &tx_result_err,
            InstructionError::ExternalAccountDataModified,
        );
    } else {
        // If we did not get a transaction error then that means that the transaction
        // was blocked because the account was found to not be delegated
        // For undelegation tests this is the case if undelegation completes before
        // we run the transaction that tried to increase the count
        macro_rules! invalid_error {
            ($tx_result_err:expr) => {
                panic!("Expected transaction or transwise NotAllWritablesDelegated error, got: {:?}", $tx_result_err)
            };
        }
        match &tx_result_err.kind {
            ErrorKind::RpcError(RpcError::RpcResponseError {
                message, ..
            }) => {
                if !message.contains("NotAllWritablesDelegated") {
                    invalid_error!(tx_result_err);
                }
            }
            _ => invalid_error!(tx_result_err),
        }
    }
}

fn assert_can_increase_committee_count(
    pda: Pubkey,
    payer: &Keypair,
    blockhash: Hash,
    chain_client: &RpcClient,
    commitment: &CommitmentConfig,
) {
    let ix = increase_count_instruction(pda);
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
    assert!(tx_res.is_ok());
}

#[test]
fn test_committed_and_undelegated_single_account_redelegation() {
    run_test!({
        let (ctx, sig, tx_res) = commit_and_undelegate_one_account(false);
        info!("{} '{:?}'", sig, tx_res);
        let ScheduleCommitTestContextFields {
            payer,
            committees,
            commitment,
            ephem_client,
            ephem_blockhash,
            ..
        } = ctx.fields();
        let chain_client = ctx.try_chain_client().unwrap();

        // 1. Show we cannot use it in the ephemeral anymore
        assert_cannot_increase_committee_count(
            committees[0].1,
            payer,
            *ephem_blockhash,
            ephem_client,
            commitment,
        );

        // 2. Wait for commit + undelegation to finish and try chain again
        {
            verify::fetch_and_verify_commit_result_from_logs(&ctx, sig);

            let blockhash = chain_client.get_latest_blockhash().unwrap();
            assert_can_increase_committee_count(
                committees[0].1,
                payer,
                blockhash,
                chain_client,
                commitment,
            );
        }

        // 3. Re-delegate the same account
        {
            std::thread::sleep(std::time::Duration::from_secs(2));
            let blockhash = chain_client.get_latest_blockhash().unwrap();
            ctx.delegate_committees(Some(blockhash)).unwrap();
        }

        // 4. Now we can modify it in the ephemeral again and no longer on chain
        {
            let ephem_blockhash = ephem_client.get_latest_blockhash().unwrap();
            assert_can_increase_committee_count(
                committees[0].1,
                payer,
                ephem_blockhash,
                ephem_client,
                commitment,
            );

            let chain_blockhash = chain_client.get_latest_blockhash().unwrap();
            assert_cannot_increase_committee_count(
                committees[0].1,
                payer,
                chain_blockhash,
                chain_client,
                commitment,
            );
        }
    });
}

// The below is the same as test_committed_and_undelegated_single_account_redelegation
// but for two accounts
#[test]
fn test_committed_and_undelegated_accounts_redelegation() {
    run_test!({
        let (ctx, sig, tx_res) = commit_and_undelegate_two_accounts(false);
        info!("{} '{:?}'", sig, tx_res);
        let ScheduleCommitTestContextFields {
            payer,
            committees,
            commitment,
            ephem_client,
            ephem_blockhash,
            ..
        } = ctx.fields();
        let chain_client = ctx.try_chain_client().unwrap();

        // 1. Show we cannot use them in the ephemeral anymore
        {
            assert_cannot_increase_committee_count(
                committees[0].1,
                payer,
                *ephem_blockhash,
                ephem_client,
                commitment,
            );
            assert_cannot_increase_committee_count(
                committees[1].1,
                payer,
                *ephem_blockhash,
                ephem_client,
                commitment,
            );
        }

        // 2. Wait for commit + undelegation to finish and try chain again
        {
            verify::fetch_and_verify_commit_result_from_logs(&ctx, sig);

            // we need a new blockhash otherwise the tx is identical to the above
            let blockhash = chain_client.get_latest_blockhash().unwrap();
            assert_can_increase_committee_count(
                committees[0].1,
                payer,
                blockhash,
                chain_client,
                commitment,
            );
            assert_can_increase_committee_count(
                committees[1].1,
                payer,
                blockhash,
                chain_client,
                commitment,
            );
        }

        // 3. Re-delegate the same accounts
        {
            std::thread::sleep(std::time::Duration::from_secs(2));
            let blockhash = chain_client.get_latest_blockhash().unwrap();
            ctx.delegate_committees(Some(blockhash)).unwrap();
        }

        // 4. Now we can modify them in the ephemeral again and no longer on chain
        {
            let ephem_blockhash = ephem_client.get_latest_blockhash().unwrap();
            assert_can_increase_committee_count(
                committees[0].1,
                payer,
                ephem_blockhash,
                ephem_client,
                commitment,
            );
            assert_can_increase_committee_count(
                committees[1].1,
                payer,
                ephem_blockhash,
                ephem_client,
                commitment,
            );

            let chain_blockhash = chain_client.get_latest_blockhash().unwrap();
            assert_cannot_increase_committee_count(
                committees[0].1,
                payer,
                chain_blockhash,
                chain_client,
                commitment,
            );
            assert_cannot_increase_committee_count(
                committees[1].1,
                payer,
                chain_blockhash,
                chain_client,
                commitment,
            );
        }
    });
}

// -----------------
// Invalid Cases
// -----------------
#[test]
fn test_committing_and_undelegating_one_account_modifying_it_after() {
    run_test!({
        let (ctx, sig, res) = commit_and_undelegate_one_account(true);
        info!("{} '{:?}'", sig, res);

        // 1. Show we cannot use them in the ephemeral anymore
        ctx.assert_ephemeral_transaction_error(
            sig,
            &res,
            "instruction modified data of an account it does not own",
        );

        // 2. Retrieve the signature of the scheduled commit sent
        let logs = ctx.fetch_ephemeral_logs(sig).unwrap();
        let sig =
            extract_scheduled_commit_sent_signature_from_logs(&logs).unwrap();

        // 3. Assert that the commit was not scheduled -> the transaction is not confirmed
        assert!(!ctx
            .ephem_client
            .as_ref()
            .unwrap()
            .confirm_transaction(&sig)
            .unwrap());
    });
}
#[test]
fn test_committing_and_undelegating_two_accounts_modifying_them_after() {
    run_test!({
        let (ctx, sig, res) = commit_and_undelegate_two_accounts(true);
        info!("{} '{:?}'", sig, res);

        // 1. Show we cannot use them in the ephemeral anymore
        ctx.assert_ephemeral_transaction_error(
            sig,
            &res,
            "instruction modified data of an account it does not own",
        );

        // 2. Retrieve the signature of the scheduled commit sent
        let logs = ctx.fetch_ephemeral_logs(sig).unwrap();
        let scheduled_commmit_sent_sig =
            extract_scheduled_commit_sent_signature_from_logs(&logs).unwrap();

        // 3. Assert that the commit was not scheduled -> the transaction is not confirmed
        assert!(!ctx
            .ephem_client
            .as_ref()
            .unwrap()
            .confirm_transaction(&scheduled_commmit_sent_sig)
            .unwrap());
    });
}
