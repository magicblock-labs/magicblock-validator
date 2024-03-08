use std::collections::HashSet;

use crate::bank::{Bank, TransactionExecutionRecordingOpts};
use crate::LAMPORTS_PER_SIGNATURE;
use log::{debug, error, info, trace, warn};
use rayon::{
    iter::IndexedParallelIterator,
    prelude::{IntoParallelIterator, IntoParallelRefIterator, ParallelIterator},
};
use solana_program_runtime::timings::ExecuteTimings;
use solana_sdk::{
    account::Account,
    clock::MAX_PROCESSING_AGE,
    instruction::{AccountMeta, Instruction},
    message::Message,
    native_token::LAMPORTS_PER_SOL,
    pubkey::Pubkey,
    rent::Rent,
    signature::Keypair,
    signer::Signer,
    stake_history::Epoch,
    system_program, system_transaction,
    sysvar::{clock, epoch_schedule, fees, last_restart_slot, recent_blockhashes, rent},
    transaction::{SanitizedTransaction, Transaction},
};

use super::elfs;

// -----------------
// Account Initialization
// -----------------
pub fn create_accounts(num: usize) -> Vec<Keypair> {
    (0..num).into_par_iter().map(|_| Keypair::new()).collect()
}

pub fn create_funded_accounts(bank: &Bank, num: usize, lamports: Option<u64>) -> Vec<Keypair> {
    let accounts = create_accounts(num);
    let lamports = lamports.unwrap_or_else(|| {
        let rent_exempt_reserve = Rent::default().minimum_balance(0);
        rent_exempt_reserve + (num as u64 * LAMPORTS_PER_SIGNATURE)
    });

    accounts.par_iter().for_each(|account| {
        bank.store_account(
            &account.pubkey(),
            &Account {
                lamports,
                data: vec![],
                owner: system_program::id(),
                executable: false,
                rent_epoch: Epoch::MAX,
            },
        );
    });

    accounts
}

// -----------------
// System Program
// -----------------
pub fn create_system_transfer_transactions(bank: &Bank, num: usize) -> Vec<SanitizedTransaction> {
    let funded_accounts = create_funded_accounts(bank, 2 * num, None);
    funded_accounts
        .into_par_iter()
        .chunks(2)
        .map(|chunk| {
            let from = &chunk[0];
            let to = &chunk[1];
            system_transaction::transfer(from, &to.pubkey(), 1, bank.last_blockhash())
        })
        .map(SanitizedTransaction::from_transaction_for_tests)
        .collect()
}

// Noop
pub fn create_noop_transaction(bank: &Bank) -> SanitizedTransaction {
    let funded_accounts = create_funded_accounts(bank, 2, None);
    let instruction = create_noop_instruction(&elfs::noop::id(), &funded_accounts);
    let message = Message::new(&[instruction], None);
    let transaction = Transaction::new_unsigned(message);
    SanitizedTransaction::try_from_legacy_transaction(transaction).unwrap()
}

fn create_noop_instruction(program_id: &Pubkey, funded_accounts: &[Keypair]) -> Instruction {
    let ix_bytes: Vec<u8> = Vec::new();
    Instruction::new_with_bytes(
        *program_id,
        &ix_bytes,
        vec![AccountMeta::new(funded_accounts[0].pubkey(), true)],
    )
}

// SolanaX
pub fn create_solx_send_post_transaction(bank: &Bank) -> (SanitizedTransaction, Pubkey, Pubkey) {
    let funded_accounts = create_funded_accounts(bank, 2, Some(LAMPORTS_PER_SOL));
    let instruction = create_solx_send_post_instruction(&elfs::solanax::id(), &funded_accounts);
    let message = Message::new(&[instruction], Some(&funded_accounts[0].pubkey()));
    let payer = &funded_accounts[0];
    let post = &funded_accounts[1];
    let transaction = Transaction::new(&[&payer, &post], message, bank.last_blockhash());
    (
        SanitizedTransaction::try_from_legacy_transaction(transaction).unwrap(),
        payer.pubkey(),
        post.pubkey(),
    )
}

fn create_solx_send_post_instruction(
    program_id: &Pubkey,
    funded_accounts: &[Keypair],
) -> Instruction {
    // https://explorer.solana.com/tx/nM2WLNPVfU3R8C4dJwhzwBsVXXgBkySAuBrGTEoaGaAQMxNHy4mnAgLER8ddDmD6tjw3suVhfG1RdbdbhyScwLK?cluster=devnet
    #[rustfmt::skip]
    let ix_bytes: Vec<u8> = vec![
        0x84, 0xf5, 0xee, 0x1d,
        0xf3, 0x2a, 0xad, 0x36,
        0x05, 0x00, 0x00, 0x00,
        0x68, 0x65, 0x6c, 0x6c,
        0x6f,
    ];
    Instruction::new_with_bytes(
        *program_id,
        &ix_bytes,
        vec![
            AccountMeta::new(funded_accounts[0].pubkey(), true),
            AccountMeta::new(funded_accounts[1].pubkey(), true),
            AccountMeta::new_readonly(system_program::id(), false),
        ],
    )
}

// Sysvars
pub fn create_sysvars_get_transaction(bank: &Bank) -> SanitizedTransaction {
    let funded_accounts = create_funded_accounts(bank, 2, None);
    let instruction = create_sysvars_get_instruction(&elfs::sysvars::id(), &funded_accounts);
    let message = Message::new(&[instruction], None);
    let transaction = Transaction::new_unsigned(message);
    SanitizedTransaction::try_from_legacy_transaction(transaction).unwrap()
}

fn create_sysvars_get_instruction(program_id: &Pubkey, funded_accounts: &[Keypair]) -> Instruction {
    let ix_bytes: Vec<u8> = vec![0x00];
    Instruction::new_with_bytes(
        *program_id,
        &ix_bytes,
        vec![AccountMeta::new(funded_accounts[0].pubkey(), true)],
    )
}

pub fn create_sysvars_from_account_transaction(bank: &Bank) -> SanitizedTransaction {
    let funded_accounts = create_funded_accounts(bank, 2, None);
    let instruction =
        create_sysvars_from_account_instruction(&elfs::sysvars::id(), &funded_accounts);
    let message = Message::new(&[instruction], None);
    let transaction = Transaction::new_unsigned(message);
    SanitizedTransaction::try_from_legacy_transaction(transaction).unwrap()
}

fn create_sysvars_from_account_instruction(
    program_id: &Pubkey,
    funded_accounts: &[Keypair],
) -> Instruction {
    let ix_bytes: Vec<u8> = vec![0x01];
    Instruction::new_with_bytes(
        *program_id,
        &ix_bytes,
        vec![
            AccountMeta::new(funded_accounts[0].pubkey(), true),
            AccountMeta::new_readonly(clock::id(), false),
            AccountMeta::new_readonly(rent::id(), false),
            AccountMeta::new_readonly(epoch_schedule::id(), false),
            #[allow(deprecated)]
            AccountMeta::new_readonly(fees::id(), false),
            #[allow(deprecated)]
            AccountMeta::new_readonly(recent_blockhashes::id(), false),
            AccountMeta::new_readonly(last_restart_slot::id(), false),
        ],
    )
}

// -----------------
// Transactions
// -----------------
pub fn execute_transactions(bank: &Bank, txs: Vec<SanitizedTransaction>) {
    let batch = bank.prepare_sanitized_batch(&txs);

    let mut timings = ExecuteTimings::default();
    let (transaction_results, transaction_balances) = bank.load_execute_and_commit_transactions(
        &batch,
        MAX_PROCESSING_AGE,
        true,
        TransactionExecutionRecordingOpts::recording_logs(),
        &mut timings,
        None,
    );

    trace!("{:#?}", txs);
    trace!("{:#?}", transaction_results.execution_results);
    trace!("{:#?}", transaction_balances);

    for res in transaction_results.execution_results.iter() {
        if let Err(err) = res.flattened_result() {
            error!(
                "Error: {:?}, ({}) 😈",
                err,
                if res.was_executed() {
                    "executed"
                } else {
                    "not executed"
                },
            );
        } else if res.was_executed_successfully() {
            info!(
                "Executed {}",
                if res.was_executed_successfully() {
                    "successfully. 😀"
                } else {
                    "but failed! 😈"
                }
            );
        } else {
            warn!("Failed to execute 😈",);
        }
    }

    for key in txs
        .iter()
        .flat_map(|tx| tx.message().account_keys().iter())
        .collect::<HashSet<_>>()
    {
        if key.eq(&system_program::id()) {
            continue;
        }

        if let Some(account) = bank.get_account(key) {
            trace!("{:?}: {:#?}", key, account);
        } else {
            debug!("{:?}: missing", key);
        }
    }

    info!("");
    info!("=============== Logs ===============");
    for res in transaction_results.execution_results.iter() {
        if let Some(logs) = res.details().as_ref().and_then(|x| x.log_messages.as_ref()) {
            for log in logs {
                info!("> {log}");
            }
        }
    }
    info!("");
}
