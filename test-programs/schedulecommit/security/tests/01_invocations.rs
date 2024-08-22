use schedulecommit_client::{
    consts::MAGIC_PROGRAM_ADDR, ScheduleCommitTestContext,
};
use schedulecommit_program::api::schedule_commit_cpi_instruction;
use solana_rpc_client_api::config::RpcSendTransactionConfig;
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signer::Signer,
    transaction::Transaction,
};

use crate::utils::{
    create_nested_schedule_cpis_instruction,
    create_sibling_non_cpi_instruction,
    create_sibling_schedule_cpis_instruction,
};
mod utils;

const _PROGRAM_ADDR: &str = "9hgprgZiRWmy8KkfvUuaVkDGrqo9GzeXMohwq6BazgUY";

const PROGRAM_ID_NOT_FOUND: &str =
    "ScheduleCommit ERR: failed to find parent program id";
const INVALID_ACCOUNT_OWNER: &str = "Invalid account owner";
const NEEDS_TO_BE_OWNED_BY_INVOKING_PROGRAM: &str =
    "needs to be owned by the invoking program";

fn prepare_ctx_with_account_to_commit() -> ScheduleCommitTestContext {
    let ctx = if std::env::var("FIXED_KP").is_ok() {
        ScheduleCommitTestContext::new(2)
    } else {
        ScheduleCommitTestContext::new_random_keys(2)
    };
    ctx.init_committees().unwrap();
    ctx.delegate_committees().unwrap();

    ctx
}

fn create_schedule_commit_ix(
    payer: Pubkey,
    magic_program_key: Pubkey,
    pubkeys: &[Pubkey],
) -> Instruction {
    let instruction_data = vec![1, 0, 0, 0];
    let mut account_metas = vec![AccountMeta::new(payer, true)];

    for pubkey in pubkeys {
        account_metas.push(AccountMeta {
            pubkey: *pubkey,
            is_signer: false,
            // NOTE: It appears they need to be writable to be properly cloned?
            is_writable: true,
        });
    }
    Instruction::new_with_bytes(
        magic_program_key,
        &instruction_data,
        account_metas,
    )
}

#[test]
fn test_schedule_commit_directly_with_single_ix() {
    // Attempts to directly commit PDAs via the MagicBlock program.
    // This fails since a CPI program id cannot be found.
    let ctx = prepare_ctx_with_account_to_commit();
    let ScheduleCommitTestContext {
        payer,
        commitment,
        committees,
        ephem_blockhash,
        ephem_client,
        ..
    } = &ctx;
    let ix = create_schedule_commit_ix(
        payer.pubkey(),
        MAGIC_PROGRAM_ADDR,
        &committees.iter().map(|(_, pda)| *pda).collect::<Vec<_>>(),
    );

    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&payer.pubkey()),
        &[&payer],
        *ephem_blockhash,
    );

    let sig = tx.signatures[0];
    let res = ephem_client
        .send_and_confirm_transaction_with_spinner_and_config(
            &tx,
            *commitment,
            RpcSendTransactionConfig {
                skip_preflight: true,
                ..Default::default()
            },
        );
    ctx.assert_ephemeral_transaction_error(sig, &res, PROGRAM_ID_NOT_FOUND);
}

#[test]
fn test_schedule_commit_directly_with_commit_ix_sandwiched() {
    // Attempts to directly commit PDAs via the MagicBlock program, however adds
    // two other instructions around the main one in order to confuse the CPI check algorithm.
    // Fails since a CPI program id cannot be found.
    let ctx = prepare_ctx_with_account_to_commit();
    let ScheduleCommitTestContext {
        payer,
        commitment,
        committees,
        ephem_blockhash,
        ephem_client,
        ..
    } = &ctx;

    // Send money to one of the PDAs since it is delegated and can be cloned
    let (_, rcvr_pda) = committees[0];

    // 1. Transfer to rcvr
    let transfer_ix_1 = solana_sdk::system_instruction::transfer(
        &payer.pubkey(),
        &rcvr_pda,
        1_000_000,
    );

    // 2. Schedule commit
    let ix = create_schedule_commit_ix(
        payer.pubkey(),
        MAGIC_PROGRAM_ADDR,
        &committees.iter().map(|(_, pda)| *pda).collect::<Vec<_>>(),
    );

    // 3. Transfer to rcvr again
    let transfer_ix_2 = solana_sdk::system_instruction::transfer(
        &payer.pubkey(),
        &rcvr_pda,
        2_000_000,
    );

    let tx = Transaction::new_signed_with_payer(
        &[transfer_ix_1, ix, transfer_ix_2],
        Some(&payer.pubkey()),
        &[&payer],
        *ephem_blockhash,
    );

    let sig = tx.signatures[0];
    let res = ephem_client
        .send_and_confirm_transaction_with_spinner_and_config(
            &tx,
            *commitment,
            RpcSendTransactionConfig {
                skip_preflight: true,
                ..Default::default()
            },
        );
    ctx.assert_ephemeral_transaction_error(sig, &res, PROGRAM_ID_NOT_FOUND);
}

#[test]
fn test_schedule_commit_via_direct_and_indirect_cpi_of_other_program() {
    // Attempts to commit PDAs via a malicious program.
    // That program commits the PDAs in two ways, first correctly via the owning program,
    // but then again directly. The second attempt should fail due to the invoking program
    // not matching the PDA's owner.
    let ctx = prepare_ctx_with_account_to_commit();
    let ScheduleCommitTestContext {
        payer,
        commitment,
        committees,
        ephem_blockhash,
        ephem_client,
        ..
    } = &ctx;

    let players = &committees
        .iter()
        .map(|(player, _)| player.pubkey())
        .collect::<Vec<_>>();
    let pdas = &committees.iter().map(|(_, pda)| *pda).collect::<Vec<_>>();

    let ix =
        create_sibling_schedule_cpis_instruction(payer.pubkey(), pdas, players);

    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&payer.pubkey()),
        &[&payer],
        *ephem_blockhash,
    );

    let sig = tx.signatures[0];
    let res = ephem_client
        .send_and_confirm_transaction_with_spinner_and_config(
            &tx,
            *commitment,
            RpcSendTransactionConfig {
                skip_preflight: true,
                ..Default::default()
            },
        );

    ctx.assert_ephemeral_transaction_error(sig, &res, INVALID_ACCOUNT_OWNER);
    ctx.assert_ephemeral_transaction_error(
        sig,
        &res,
        NEEDS_TO_BE_OWNED_BY_INVOKING_PROGRAM,
    );
}

#[test]
fn test_schedule_commit_via_direct_and_from_other_program_indirect_cpi_including_non_cpi_instruction(
) {
    // Combines three instructions into one transaction:
    // - a non-CPI instruction doing nothing
    // - a CPI instruction which invokes the program owning the PDAs and is legit
    // - a CPI instruction to a malicious program which attempts to commit the PDAs
    //   directly via the MagicBlock program
    // The last one fails due to it not owning the PDAs.
    let ctx = prepare_ctx_with_account_to_commit();
    let ScheduleCommitTestContext {
        payer,
        commitment,
        committees,
        ephem_blockhash,
        ephem_client,
        ..
    } = &ctx;

    let players = &committees
        .iter()
        .map(|(player, _)| player.pubkey())
        .collect::<Vec<_>>();
    let pdas = &committees.iter().map(|(_, pda)| *pda).collect::<Vec<_>>();

    let non_cpi_ix = create_sibling_non_cpi_instruction(payer.pubkey());

    let cpi_ix = schedule_commit_cpi_instruction(
        payer.pubkey(),
        MAGIC_PROGRAM_ADDR,
        players,
        pdas,
    );

    let nested_cpi_ix =
        create_nested_schedule_cpis_instruction(payer.pubkey(), pdas, players);

    let tx = Transaction::new_signed_with_payer(
        &[non_cpi_ix, cpi_ix, nested_cpi_ix],
        Some(&payer.pubkey()),
        &[&payer],
        *ephem_blockhash,
    );

    let sig = tx.signatures[0];
    let res = ephem_client
        .send_and_confirm_transaction_with_spinner_and_config(
            &tx,
            *commitment,
            RpcSendTransactionConfig {
                skip_preflight: true,
                ..Default::default()
            },
        );

    ctx.assert_ephemeral_transaction_error(sig, &res, INVALID_ACCOUNT_OWNER);
    ctx.assert_ephemeral_transaction_error(
        sig,
        &res,
        NEEDS_TO_BE_OWNED_BY_INVOKING_PROGRAM,
    );
}
