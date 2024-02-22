use std::collections::HashSet;

use log::{debug, info, trace};
use rayon::{
    iter::IndexedParallelIterator,
    prelude::{IntoParallelIterator, IntoParallelRefIterator, ParallelIterator},
};
use sleipnir_bank::bank::{Bank, TransactionExecutionRecordingOpts};
use sleipnir_bank::LAMPORTS_PER_SIGNATURE;
use solana_program_runtime::timings::ExecuteTimings;
use solana_sdk::{
    account::Account,
    clock::MAX_PROCESSING_AGE,
    instruction::{AccountMeta, Instruction},
    message::Message,
    pubkey::Pubkey,
    rent::Rent,
    signature::Keypair,
    signer::Signer,
    stake_history::Epoch,
    system_program, system_transaction,
    transaction::{SanitizedTransaction, Transaction},
};
pub(crate) mod elfs;

pub fn init_logger() {
    let _ = env_logger::builder().is_test(true).try_init();
}

pub fn create_accounts(num: usize) -> Vec<Keypair> {
    (0..num).into_par_iter().map(|_| Keypair::new()).collect()
}

pub fn create_funded_accounts(bank: &Bank, num: usize) -> Vec<Keypair> {
    assert!(
        num.is_power_of_two(),
        "must be power of 2 for parallel funding tree"
    );
    let accounts = create_accounts(num);
    let rent_exempt_reserve = Rent::default().minimum_balance(0);
    eprintln!("rent_exempt_reserve: {}", rent_exempt_reserve);

    accounts.par_iter().for_each(|account| {
        bank.store_account(
            &account.pubkey(),
            &Account {
                lamports: rent_exempt_reserve + (num as u64 * LAMPORTS_PER_SIGNATURE),
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
    let funded_accounts = create_funded_accounts(bank, 2 * num);
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

// -----------------
// ELF Programs
// -----------------
pub fn add_elf_programs(bank: &Bank) {
    for (program_id, account) in elfs::elf_accounts() {
        bank.store_account(&program_id, &account);
    }
}

pub fn add_elf_program(bank: &Bank, program_id: &Pubkey) {
    let program_accs = elfs::elf_accounts_for(program_id);
    if program_accs.is_empty() {
        panic!("Unknown ELF account: {:?}", program_id);
    }

    for (acc_id, account) in program_accs {
        debug!("Adding ELF program: '{}'", acc_id);
        bank.store_account(&acc_id, &account);
    }
}

pub fn create_noop_transaction(bank: &Bank) -> SanitizedTransaction {
    let funded_accounts = create_funded_accounts(bank, 2);
    let instruction = create_noop_instruction(&elfs::noop::id(), &funded_accounts);
    let message = Message::new(&[instruction], None);
    let transaction = Transaction::new_unsigned(message);
    SanitizedTransaction::from_transaction_for_tests(transaction)
}

fn create_noop_instruction(program_id: &Pubkey, funded_accounts: &[Keypair]) -> Instruction {
    let ix_bytes: Vec<u8> = Vec::new();
    Instruction::new_with_bytes(
        *program_id,
        &ix_bytes,
        vec![AccountMeta::new(funded_accounts[0].pubkey(), true)],
    )
}

pub fn create_solx_send_post_transaction(bank: &Bank) -> SanitizedTransaction {
    let funded_accounts = create_funded_accounts(bank, 2);
    let instruction = create_solx_send_post_instruction(&elfs::solanax::id(), &funded_accounts);
    let transaction = Transaction::new_signed_with_payer(
        &[instruction],
        Some(&funded_accounts[0].pubkey()),
        &[&funded_accounts[0]],
        bank.last_blockhash(),
    );
    SanitizedTransaction::from_transaction_for_tests(transaction)
}

fn create_solx_send_post_instruction(
    program_id: &Pubkey,
    funded_accounts: &[Keypair],
) -> Instruction {
    let ix_bytes: Vec<u8> = Vec::new();
    Instruction::new_with_bytes(
        *program_id,
        &ix_bytes,
        vec![
            AccountMeta::new(funded_accounts[0].pubkey(), true),
            AccountMeta::new(funded_accounts[1].pubkey(), false),
            AccountMeta::new_readonly(system_program::id(), false),
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
    debug!("{:#?}", transaction_balances);

    for key in txs
        .iter()
        .flat_map(|tx| tx.message().account_keys().iter())
        .collect::<HashSet<_>>()
    {
        if key.eq(&system_program::id()) {
            continue;
        }

        let account = bank.get_account(key).unwrap();

        debug!("{:?}: {:#?}", key, account);
    }

    info!("=============== Logs ===============");
    for res in transaction_results.execution_results.iter() {
        if let Some(logs) = res.details().as_ref().and_then(|x| x.log_messages.as_ref()) {
            for log in logs {
                info!("> {log}");
            }
        }
    }
}
