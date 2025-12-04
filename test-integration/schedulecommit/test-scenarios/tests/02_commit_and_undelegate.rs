use integration_test_tools::{
    conversions::stringify_simulation_result, run_test,
    scheduled_commits::extract_scheduled_commit_sent_signature_from_logs,
    transactions::send_and_confirm_instructions_with_payer,
};
use log::*;
use program_schedulecommit::api::{
    increase_count_instruction, schedule_commit_and_undelegate_cpi_instruction,
    schedule_commit_and_undelegate_cpi_twice,
    schedule_commit_and_undelegate_cpi_with_mod_after_instruction,
    set_count_instruction,
};
use program_schedulecommit::FAIL_UNDELEGATION_COUNT;
use schedulecommit_client::{
    verify, ScheduleCommitTestContext, ScheduleCommitTestContextFields,
};
use solana_rpc_client::rpc_client::{RpcClient, SerializableTransaction};
use solana_rpc_client_api::{
    client_error::Error as ClientError, config::RpcSendTransactionConfig,
};
use solana_sdk::{
    commitment_config::CommitmentConfig,
    instruction::InstructionError,
    pubkey::Pubkey,
    signature::{Keypair, Signature},
    signer::Signer,
    transaction::Transaction,
};
use test_kit::init_logger;
use utils::{
    assert_one_committee_account_was_undelegated_on_chain,
    assert_one_committee_synchronized_count,
    assert_one_committee_was_committed,
    assert_two_committee_accounts_were_undelegated_on_chain,
    assert_two_committees_synchronized_count,
    assert_two_committees_were_committed, extract_transaction_error,
    get_context_with_delegated_committees,
};

use crate::utils::assert_is_one_of_instruction_errors;

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
        payer_ephem: payer,
        committees,
        commitment,
        ephem_client,
        ..
    } = ctx.fields();

    let ix = if modify_after {
        schedule_commit_and_undelegate_cpi_with_mod_after_instruction(
            payer.pubkey(),
            magicblock_magic_program_api::id(),
            magicblock_magic_program_api::MAGIC_CONTEXT_PUBKEY,
            &committees
                .iter()
                .map(|(player, _)| player.pubkey())
                .collect::<Vec<_>>(),
            &committees.iter().map(|(_, pda)| *pda).collect::<Vec<_>>(),
        )
    } else {
        schedule_commit_and_undelegate_cpi_instruction(
            payer.pubkey(),
            magicblock_magic_program_api::id(),
            magicblock_magic_program_api::MAGIC_CONTEXT_PUBKEY,
            &committees
                .iter()
                .map(|(player, _)| player.pubkey())
                .collect::<Vec<_>>(),
            &committees.iter().map(|(_, pda)| *pda).collect::<Vec<_>>(),
        )
    };
    let ephem_blockhash = ephem_client.get_latest_blockhash().unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&payer.pubkey()),
        &[&payer],
        ephem_blockhash,
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
        payer_ephem: payer,
        committees,
        commitment,
        ephem_client,
        ..
    } = ctx.fields();

    let ix = if modify_after {
        schedule_commit_and_undelegate_cpi_with_mod_after_instruction(
            payer.pubkey(),
            magicblock_magic_program_api::id(),
            magicblock_magic_program_api::MAGIC_CONTEXT_PUBKEY,
            &committees
                .iter()
                .map(|(player, _)| player.pubkey())
                .collect::<Vec<_>>(),
            &committees.iter().map(|(_, pda)| *pda).collect::<Vec<_>>(),
        )
    } else {
        schedule_commit_and_undelegate_cpi_instruction(
            payer.pubkey(),
            magicblock_magic_program_api::id(),
            magicblock_magic_program_api::MAGIC_CONTEXT_PUBKEY,
            &committees
                .iter()
                .map(|(player, _)| player.pubkey())
                .collect::<Vec<_>>(),
            &committees.iter().map(|(_, pda)| *pda).collect::<Vec<_>>(),
        )
    };

    let ephem_blockhash = ephem_client.get_latest_blockhash().unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&payer.pubkey()),
        &[&payer],
        ephem_blockhash,
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

fn commit_and_undelegate_two_accounts_twice() -> (
    ScheduleCommitTestContext,
    Signature,
    Result<Signature, ClientError>,
) {
    let ctx = get_context_with_delegated_committees(2);
    let ScheduleCommitTestContextFields {
        payer_ephem: payer,
        committees,
        commitment,
        ephem_client,
        ..
    } = ctx.fields();

    let ix = schedule_commit_and_undelegate_cpi_twice(
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
    rpc_client: &RpcClient,
) {
    // NOTE: in the case of checking this on the ephemeral there are two reasons why an account
    //       cannot be modified in case it was _just_ undelegted:
    //
    // - it's owner is set to the delegation program and thus the transaction fails when it runs
    //   - this is the case when the undelegation is still in progress and/or the validator has not
    //     yet seen the resulting on chain account update
    // - the undelegation already went through and the validator saw this update
    //   - in this case the account was marked as undelegated

    let ix = increase_count_instruction(pda);
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&payer.pubkey()),
        &[payer],
        rpc_client.get_latest_blockhash().unwrap(),
    );
    let simulation_result = rpc_client.simulate_transaction(&tx).unwrap();
    let simulation =
        stringify_simulation_result(simulation_result.value, &tx.signatures[0]);
    debug!(
        "{}\nExpecting ExternalAccountDataModified | ProgramFailedToComplete ({})",
        simulation,
        rpc_client.url()
    );

    // In case the account is undelegated in the ephem we see this when simulating.
    // Since in this case the transaction never lands it cannot be confirmed and
    // times out eventually. Until that is fixed we shortcut here and accept simulation
    // failing that way as a good enough indicator that an account is undelegated and
    // cannot be modified.
    if simulation.contains("InvalidWritableAccount") {
        return;
    }

    let tx_res = rpc_client
        .send_and_confirm_transaction_with_spinner_and_config(
            &tx,
            rpc_client.commitment(),
            RpcSendTransactionConfig {
                skip_preflight: true,
                ..Default::default()
            },
        );
    let (tx_result_err, tx_err) = extract_transaction_error(tx_res);
    if let Some(tx_err) = tx_err {
        assert_is_one_of_instruction_errors(
            tx_err,
            &tx_result_err,
            InstructionError::ExternalAccountDataModified,
            // Recently we saw the following when the account is owned by the delegation program
            // and serialized:
            //   Program failed: Access violation in input section at address 0x400000060 of size 32
            //   Error: InstructionError(0, ProgramFailedToComplete)
            InstructionError::ProgramFailedToComplete,
        );
    } else {
        panic!(
            "Transaction {} should have failed ({})",
            tx.signatures[0],
            rpc_client.url()
        );
    }
}

fn assert_can_increase_committee_count(
    pda: Pubkey,
    payer: &Keypair,
    rpc_client: &RpcClient,
    commitment: &CommitmentConfig,
) {
    let ix = increase_count_instruction(pda);
    let tx_res = send_and_confirm_instructions_with_payer(
        rpc_client,
        &[ix],
        payer,
        *commitment,
        "assert_can_increase_committee_count",
    );

    if let Err(err) = &tx_res {
        error!("Failed to increase count: {:?} ({})", err, rpc_client.url());
    }
    assert!(tx_res.is_ok());
}

#[test]
fn test_committed_and_undelegated_single_account_redelegation() {
    run_test!({
        let (ctx, sig, tx_res) = commit_and_undelegate_one_account(false);
        debug!(
            "✅ Committed and undelegated account {} '{:?}'",
            sig, tx_res
        );
        let ScheduleCommitTestContextFields {
            payer_ephem,
            payer_chain,
            committees,
            commitment,
            ephem_client,
            ..
        } = ctx.fields();
        let chain_client = ctx.try_chain_client().unwrap();

        // 1. Show we cannot use it in the ephemeral anymore
        assert_cannot_increase_committee_count(
            committees[0].1,
            payer_ephem,
            ephem_client,
        );
        debug!("✅ Cannot increase count in ephemeral after undelegation triggered");

        // 2. Wait for commit + undelegation to finish and try chain again
        {
            verify::fetch_and_verify_commit_result_from_logs(&ctx, sig);
            debug!("Undelegation verified from logs");

            assert_can_increase_committee_count(
                committees[0].1,
                payer_chain,
                chain_client,
                commitment,
            );
            debug!(
                "✅ Can increase count on chain after undelegation completed"
            );
        }

        // 3. Re-delegate the same account
        {
            std::thread::sleep(std::time::Duration::from_secs(2));
            ctx.delegate_committees().unwrap();
            debug!("✅ Redelegated committees");
        }

        // 4. Now we can modify it in the ephemeral again and no longer on chain
        {
            assert_cannot_increase_committee_count(
                committees[0].1,
                payer_chain,
                chain_client,
            );
            debug!("✅ Cannot increase count on chain after redelegation");

            assert_can_increase_committee_count(
                committees[0].1,
                payer_ephem,
                ephem_client,
                commitment,
            );
            debug!("✅ Can increase count in ephemeral after redelegation");
        }
    });
}

// The below is the same as test_committed_and_undelegated_single_account_redelegation
// but for two accounts
#[test]
fn test_committed_and_undelegated_accounts_redelegation() {
    run_test!({
        let (ctx, sig, tx_res) = commit_and_undelegate_two_accounts(false);
        debug!(
            "✅ Committed and undelegated accounts {} '{:?}'",
            sig, tx_res
        );
        let ScheduleCommitTestContextFields {
            payer_ephem,
            payer_chain,
            committees,
            commitment,
            ephem_client,
            ..
        } = ctx.fields();
        let chain_client = ctx.try_chain_client().unwrap();

        // 1. Show we cannot use them in the ephemeral anymore
        {
            assert_cannot_increase_committee_count(
                committees[0].1,
                payer_ephem,
                ephem_client,
            );
            assert_cannot_increase_committee_count(
                committees[1].1,
                payer_ephem,
                ephem_client,
            );
            debug!("✅ Cannot increase counts in ephemeral after undelegation triggered");
        }

        // 2. Wait for commit + undelegation to finish and try chain again
        {
            verify::fetch_and_verify_commit_result_from_logs(&ctx, sig);

            // we need a new blockhash otherwise the tx is identical to the above
            assert_can_increase_committee_count(
                committees[0].1,
                payer_chain,
                chain_client,
                commitment,
            );
            assert_can_increase_committee_count(
                committees[1].1,
                payer_chain,
                chain_client,
                commitment,
            );
            debug!(
                "✅ Can increase counts on chain after undelegation completed"
            );
        }

        // 3. Re-delegate the same accounts
        {
            std::thread::sleep(std::time::Duration::from_secs(2));
            ctx.delegate_committees().unwrap();
            debug!("✅ Redelegated committees");
        }

        // 4. Now we can modify them in the ephemeral again and no longer on chain
        {
            assert_cannot_increase_committee_count(
                committees[0].1,
                payer_chain,
                chain_client,
            );
            assert_cannot_increase_committee_count(
                committees[1].1,
                payer_chain,
                chain_client,
            );
            debug!("✅ Cannot increase counts on chain after redelegation");

            assert_can_increase_committee_count(
                committees[0].1,
                payer_ephem,
                ephem_client,
                commitment,
            );
            assert_can_increase_committee_count(
                committees[1].1,
                payer_ephem,
                ephem_client,
                commitment,
            );
            debug!("✅ Can increase counts in ephemeral after redelegation");
        }
    });
}

// -----------------
// Invalid Cases
// -----------------
#[test]
fn test_committing_and_undelegating_one_account_modifying_it_after() {
    run_test!({
        let (ctx, sig, tx_res) = commit_and_undelegate_one_account(true);
        debug!(
            "✅ Committed and undelegated account and tried to mod after {} '{:?}'",
            sig, tx_res
        );

        // 1. Show we cannot use them in the ephemeral anymore
        ctx.assert_ephemeral_transaction_error(
            sig,
            &tx_res,
            "instruction modified data of an account it does not own",
        );
        debug!("✅ Verified we could not increase count in same tx that triggered undelegation in ephem");

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
        debug!("✅ Verified that not commit was scheduled since tx failed");
    });
}

#[test]
fn test_committing_and_undelegating_two_accounts_modifying_them_after() {
    run_test!({
        let (ctx, sig, tx_res) = commit_and_undelegate_two_accounts(true);
        debug!(
            "✅ Committed and undelegated accounts and tried to mod after {} '{:?}'",
            sig, tx_res
        );

        // 1. Show we cannot use them in the ephemeral anymore
        ctx.assert_ephemeral_transaction_error(
            sig,
            &tx_res,
            "instruction modified data of an account it does not own",
        );
        debug!("✅ Verified we could not increase counts in same tx that triggered undelegation in ephem");

        // 2. Retrieve the signature of the scheduled commit sent
        let logs = ctx.fetch_ephemeral_logs(sig).unwrap();
        let scheduled_commmit_sent_sig =
            extract_scheduled_commit_sent_signature_from_logs(&logs).unwrap();

        // 3. Assert that the commit was not scheduled -> the transaction is not confirmed
        debug!("Verifying that commit was not scheduled: {scheduled_commmit_sent_sig}");
        assert!(!ctx
            .ephem_client
            .as_ref()
            .unwrap()
            .confirm_transaction(&scheduled_commmit_sent_sig)
            .unwrap());
        debug!("✅ Verified that not commit was scheduled since tx failed");
    });
}

#[test]
fn test_committing_and_undelegating_two_accounts_twice() {
    run_test!({
        let (ctx, sig, tx_res) = commit_and_undelegate_two_accounts_twice();
        debug!(
            "✅ Committed and undelegated accounts and tried to mod after {} '{:?}'",
            sig, tx_res
        );

        // 1. Show we cannot use them in the ephemeral anymore
        ctx.assert_ephemeral_transaction_error(
            sig,
            &tx_res,
            "is required to be writable and delegated in order to be undelegated",
        );
        debug!("✅ Verified we could not increase counts in same tx that triggered undelegation in ephem");

        // 2. Retrieve the signature of the scheduled commit sent
        let logs = ctx.fetch_ephemeral_logs(sig).unwrap();
        let scheduled_commmit_sent_sig =
            extract_scheduled_commit_sent_signature_from_logs(&logs).unwrap();

        // 3. Assert that the commit was not scheduled -> the transaction is not confirmed
        debug!("Verifying that commit was not scheduled: {scheduled_commmit_sent_sig}");
        assert!(!ctx
            .confirm_transaction_ephem(&scheduled_commmit_sent_sig, None)
            .unwrap());
        debug!("✅ Verified that not commit was scheduled since tx failed");
    });
}

#[test]
fn test_committing_after_failed_undelegation() {
    run_test!({
        let ctx = get_context_with_delegated_committees(1);
        let ScheduleCommitTestContextFields {
            payer_ephem: payer,
            committees,
            commitment,
            ephem_client,
            ..
        } = ctx.fields();
        let [(_committee_authority, committee_pda)] = committees.as_slice()
        else {
            panic!("Unexpected num of committees");
        };

        let set_counter = |counter| {
            let ix = set_count_instruction(*committee_pda, counter);
            let ephem_blockhash = ephem_client.get_latest_blockhash().unwrap();
            let tx = Transaction::new_signed_with_payer(
                &[ix],
                Some(&payer.pubkey()),
                &[&payer],
                ephem_blockhash,
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

            debug!("set_counter sigs: {:?}", sig);
            if let Err(err) = tx_res {
                panic!("failed to set counter: {:?}", err);
            }
            assert!(ctx.confirm_transaction_ephem(&sig, None).unwrap());
        };

        std::thread::sleep(std::time::Duration::from_secs(5));

        // Set counter to FAIL_UNDELEGATION_COUNT so undelegation fails
        // NOTE: undelegation will get patched so as result account will be only committed
        set_counter(FAIL_UNDELEGATION_COUNT);

        // Commit & Undelegate
        {
            let ix = schedule_commit_and_undelegate_cpi_instruction(
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

            // 2. Retrieve the signature of the scheduled commit sent
            let logs = ctx.fetch_ephemeral_logs(*sig).unwrap();
            let scheduled_commmit_sent_sig =
                extract_scheduled_commit_sent_signature_from_logs(&logs)
                    .unwrap();

            debug!("sent_sig: {}", scheduled_commmit_sent_sig);
            // 3. Confirm commit was scheduled
            assert!(ctx
                .confirm_transaction_ephem(&scheduled_commmit_sent_sig, None)
                .unwrap());
        }

        std::thread::sleep(std::time::Duration::from_secs(5));

        let _ = ctx.fetch_ephem_account(*committee_pda);
        set_counter(2222);
        debug!(
            "payer:    {} \n \
               committee: {committee_pda}",
            payer.pubkey()
        );

        // We can continue using account on ER!!!
        // for i in 0..10 {
        //     set_counter(i);
        // }
    });
}
