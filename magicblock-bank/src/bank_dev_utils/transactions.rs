use itertools::izip;
use rayon::{
    iter::IndexedParallelIterator,
    prelude::{
        IntoParallelIterator, IntoParallelRefIterator, ParallelIterator,
    },
};
use solana_sdk::{
    account::Account,
    hash::Hash,
    instruction::{AccountMeta, Instruction},
    message::{v0::LoadedAddresses, Message},
    native_token::LAMPORTS_PER_SOL,
    pubkey::Pubkey,
    rent::Rent,
    signature::Keypair,
    signer::Signer,
    stake_history::Epoch,
    system_instruction, system_program, system_transaction,
    sysvar::{
        self, clock, epoch_schedule, fees, last_restart_slot,
        recent_blockhashes, rent,
    },
    transaction::{
        SanitizedTransaction, Transaction, TransactionError,
        VersionedTransaction,
    },
};
use solana_svm::{
    transaction_commit_result::CommittedTransaction,
    transaction_processor::ExecutionRecordingConfig,
};
use solana_timings::ExecuteTimings;
use solana_transaction_status::{
    map_inner_instructions, ConfirmedTransactionWithStatusMeta,
    TransactionStatusMeta, TransactionWithStatusMeta,
    VersionedTransactionWithStatusMeta,
};

use super::elfs;
use crate::{
    bank::Bank, transaction_results::TransactionBalancesSet,
    LAMPORTS_PER_SIGNATURE,
};

// -----------------
// Account Initialization
// -----------------
pub fn create_accounts(num: usize) -> Vec<Keypair> {
    (0..num).into_par_iter().map(|_| Keypair::new()).collect()
}

pub fn create_funded_account(bank: &Bank, lamports: Option<u64>) -> Keypair {
    let account = Keypair::new();
    let lamports = lamports.unwrap_or_else(|| {
        let rent_exempt_reserve = Rent::default().minimum_balance(0);
        rent_exempt_reserve + LAMPORTS_PER_SIGNATURE
    });

    bank.store_account(
        account.pubkey(),
        Account {
            lamports,
            data: vec![],
            owner: system_program::id(),
            executable: false,
            rent_epoch: Epoch::MAX,
        }
        .into(),
    );

    account
}

pub fn create_funded_accounts(
    bank: &Bank,
    num: usize,
    lamports: Option<u64>,
) -> Vec<Keypair> {
    let accounts = create_accounts(num);
    let lamports = lamports.unwrap_or_else(|| {
        let rent_exempt_reserve = Rent::default().minimum_balance(0);
        rent_exempt_reserve + (num as u64 * LAMPORTS_PER_SIGNATURE)
    });

    accounts.par_iter().for_each(|account| {
        bank.store_account(
            account.pubkey(),
            Account {
                lamports,
                data: vec![],
                owner: system_program::id(),
                executable: false,
                rent_epoch: Epoch::MAX,
            }
            .into(),
        );
    });

    accounts
}

// -----------------
// System Program
// -----------------
pub fn create_system_transfer_transaction(
    bank: &Bank,
    fund_lamports: u64,
    send_lamports: u64,
) -> (SanitizedTransaction, Pubkey, Pubkey) {
    let from = create_funded_account(bank, Some(fund_lamports));
    let to = Pubkey::new_unique();
    let tx = system_transaction::transfer(
        &from,
        &to,
        send_lamports,
        bank.last_blockhash(),
    );
    (
        SanitizedTransaction::from_transaction_for_tests(tx),
        from.pubkey(),
        to,
    )
}

pub fn create_system_transfer_transactions(
    bank: &Bank,
    num: usize,
) -> Vec<SanitizedTransaction> {
    let funded_accounts = create_funded_accounts(bank, 2 * num, None);
    funded_accounts
        .into_par_iter()
        .chunks(2)
        .map(|chunk| {
            let from = &chunk[0];
            let to = &chunk[1];
            system_transaction::transfer(
                from,
                &to.pubkey(),
                1,
                bank.last_blockhash(),
            )
        })
        .map(SanitizedTransaction::from_transaction_for_tests)
        .collect()
}

pub fn create_system_allocate_transaction(
    bank: &Bank,
    fund_lamports: u64,
    space: u64,
) -> (SanitizedTransaction, Pubkey, Pubkey) {
    let payer = create_funded_account(bank, Some(fund_lamports));
    let rent_exempt_reserve = Rent::default().minimum_balance(space as usize);
    let account = create_funded_account(bank, Some(rent_exempt_reserve));
    let tx = system_transaction::allocate(
        &payer,
        &account,
        bank.last_blockhash(),
        space,
    );
    (
        SanitizedTransaction::from_transaction_for_tests(tx),
        payer.pubkey(),
        account.pubkey(),
    )
}

// Noop
pub fn create_noop_transaction(
    bank: &Bank,
    recent_blockhash: Hash,
) -> SanitizedTransaction {
    let funded_accounts = create_funded_accounts(bank, 2, None);
    let instruction =
        create_noop_instruction(&elfs::noop::id(), &funded_accounts);
    let message = Message::new(&[instruction], None);
    let transaction =
        Transaction::new(&[&funded_accounts[0]], message, recent_blockhash);
    SanitizedTransaction::try_from_legacy_transaction(
        transaction,
        &Default::default(),
    )
    .unwrap()
}

fn create_noop_instruction(
    program_id: &Pubkey,
    funded_accounts: &[Keypair],
) -> Instruction {
    let ix_bytes: Vec<u8> = Vec::new();
    Instruction::new_with_bytes(
        *program_id,
        &ix_bytes,
        vec![AccountMeta::new(funded_accounts[0].pubkey(), true)],
    )
}

// SolanaX
pub struct SolanaxPostAccounts {
    pub post: Pubkey,
    pub author: Pubkey,
}
pub fn create_solx_send_post_transaction(
    bank: &Bank,
) -> (SanitizedTransaction, SolanaxPostAccounts) {
    let accounts = vec![
        create_funded_account(
            bank,
            Some(Rent::default().minimum_balance(1180)),
        ),
        create_funded_account(bank, Some(LAMPORTS_PER_SOL)),
    ];
    let post = &accounts[0];
    let author = &accounts[1];
    let instruction =
        create_solx_send_post_instruction(&elfs::solanax::id(), &accounts);
    let message = Message::new(&[instruction], Some(&author.pubkey()));
    let transaction =
        Transaction::new(&[author, post], message, bank.last_blockhash());
    (
        SanitizedTransaction::try_from_legacy_transaction(
            transaction,
            &Default::default(),
        )
        .unwrap(),
        SolanaxPostAccounts {
            post: post.pubkey(),
            author: author.pubkey(),
        },
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
    let instruction =
        create_sysvars_get_instruction(&elfs::sysvars::id(), &funded_accounts);
    let message = Message::new(&[instruction], None);
    let transaction = Transaction::new(
        &[&funded_accounts[0]],
        message,
        bank.last_blockhash(),
    );
    SanitizedTransaction::try_from_legacy_transaction(
        transaction,
        &Default::default(),
    )
    .unwrap()
}

fn create_sysvars_get_instruction(
    program_id: &Pubkey,
    funded_accounts: &[Keypair],
) -> Instruction {
    let ix_bytes: Vec<u8> = vec![0x00];
    Instruction::new_with_bytes(
        *program_id,
        &ix_bytes,
        vec![AccountMeta::new(funded_accounts[0].pubkey(), true)],
    )
}

pub fn create_sysvars_from_account_transaction(
    bank: &Bank,
) -> SanitizedTransaction {
    // This instruction checks for relative instructions
    // which is why we need to add them around the sysvar instruction

    let payer = create_funded_account(bank, Some(LAMPORTS_PER_SOL));

    // 1. System Transfer Instruction before Sysvar Instruction
    let transfer_to = Pubkey::new_unique();
    let transfer_ix = system_instruction::transfer(
        &payer.pubkey(),
        &transfer_to,
        LAMPORTS_PER_SOL / 10,
    );

    // 2. Sysvar Instruction
    let sysvar_ix = create_sysvars_from_account_instruction(
        &elfs::sysvars::id(),
        &payer.pubkey(),
    );

    // 3. System Allocate Instruction after Sysvar Instruction
    let allocate_to = Keypair::new();
    let allocate_ix = system_instruction::allocate(&allocate_to.pubkey(), 99);

    // 4. Run all Instructions as part of one Transaction
    let message = Message::new(
        &[transfer_ix, sysvar_ix, allocate_ix],
        Some(&payer.pubkey()),
    );
    let transaction = Transaction::new(
        &[&payer, &allocate_to],
        message,
        bank.last_blockhash(),
    );
    SanitizedTransaction::try_from_legacy_transaction(
        transaction,
        &Default::default(),
    )
    .unwrap()
}

fn create_sysvars_from_account_instruction(
    program_id: &Pubkey,
    payer: &Pubkey,
) -> Instruction {
    let ix_bytes: Vec<u8> = vec![0x01];
    Instruction::new_with_bytes(
        *program_id,
        &ix_bytes,
        vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new_readonly(clock::id(), false),
            AccountMeta::new_readonly(rent::id(), false),
            AccountMeta::new_readonly(epoch_schedule::id(), false),
            #[allow(deprecated)]
            AccountMeta::new_readonly(fees::id(), false),
            #[allow(deprecated)]
            AccountMeta::new_readonly(recent_blockhashes::id(), false),
            AccountMeta::new_readonly(last_restart_slot::id(), false),
            AccountMeta::new_readonly(sysvar::instructions::id(), false),
            AccountMeta::new_readonly(sysvar::slot_hashes::id(), false),
            AccountMeta::new_readonly(sysvar::slot_history::id(), false),
        ],
    )
}

// -----------------
// Transactions
// -----------------
pub fn execute_transactions(
    bank: &Bank,
    txs: Vec<Transaction>,
) -> Vec<Result<ConfirmedTransactionWithStatusMeta, TransactionError>> {
    let batch = bank.prepare_batch_for_tests(txs.clone());
    let mut timings = ExecuteTimings::default();
    let (
        commit_results,
        TransactionBalancesSet {
            pre_balances,
            post_balances,
            ..
        },
    ) = bank.load_execute_and_commit_transactions(
        &batch,
        true,
        ExecutionRecordingConfig::new_single_setting(true),
        &mut timings,
        None,
    );

    izip!(
        txs.iter(),
        commit_results.into_iter(),
        pre_balances.into_iter(),
        post_balances.into_iter(),
    )
    .map(|(tx, commit_result, pre_balances, post_balances)| {
        commit_result.map(|committed_tx| {
            let CommittedTransaction {
                status,
                log_messages,
                inner_instructions,
                return_data,
                executed_units,
                fee_details,
                ..
            } = committed_tx;

            let inner_instructions =
                inner_instructions.map(|inner_instructions| {
                    map_inner_instructions(inner_instructions).collect()
                });

            let tx_status_meta = TransactionStatusMeta {
                status,
                fee: fee_details.total_fee(),
                pre_balances,
                post_balances,
                pre_token_balances: None,
                post_token_balances: None,
                inner_instructions,
                log_messages,
                rewards: None,
                loaded_addresses: LoadedAddresses::default(),
                return_data,
                compute_units_consumed: Some(executed_units),
            };

            ConfirmedTransactionWithStatusMeta {
                slot: bank.slot(),
                tx_with_meta: TransactionWithStatusMeta::Complete(
                    VersionedTransactionWithStatusMeta {
                        transaction: VersionedTransaction::from(tx.clone()),
                        meta: tx_status_meta,
                    },
                ),
                block_time: None,
            }
        })
    })
    .collect()
}
