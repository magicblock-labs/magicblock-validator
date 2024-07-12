#![cfg(feature = "dev-context-only-utils")]

use assert_matches::assert_matches;
use sleipnir_bank::{
    bank::Bank,
    bank_dev_utils::{
        elfs::{self, add_elf_program},
        transactions::{
            create_noop_transaction, create_solx_send_post_transaction,
            create_system_allocate_transaction,
            create_system_transfer_transaction,
            create_sysvars_from_account_transaction,
            create_sysvars_get_transaction, execute_transactions,
            SolanaxPostAccounts,
        },
    },
    transaction_results::TransactionBalancesSet,
    LAMPORTS_PER_SIGNATURE,
};
use solana_sdk::{
    account::ReadableAccount, fee_calculator::FeeRateGovernor,
    genesis_config::create_genesis_config, hash::Hash,
    native_token::LAMPORTS_PER_SOL, rent::Rent,
    transaction::SanitizedTransaction,
};
use test_tools_core::init_logger;

#[test]
fn test_bank_system_transfer_instruction() {
    init_logger!();

    let (mut genesis_config, _) = create_genesis_config(u64::MAX);
    genesis_config.fee_rate_governor =
        FeeRateGovernor::new(LAMPORTS_PER_SIGNATURE, 0);

    let bank = Bank::new_for_tests(&genesis_config, None, None);

    let (tx, from, to) = create_system_transfer_transaction(
        &bank,
        LAMPORTS_PER_SOL,
        LAMPORTS_PER_SOL / 5,
    );
    let (results, balances) = execute_transactions(&bank, vec![tx]);

    const FROM_AFTER_BALANCE: u64 =
        LAMPORTS_PER_SOL - LAMPORTS_PER_SOL / 5 - LAMPORTS_PER_SIGNATURE;
    const TO_AFTER_BALANCE: u64 = LAMPORTS_PER_SOL / 5;

    // Result
    let result = &results.execution_results[0];
    assert_matches!(result.details().unwrap().status, Ok(()));

    // Accounts
    let from_acc = bank.get_account(&from).unwrap();
    let to_acc = bank.get_account(&to).unwrap();

    assert_eq!(from_acc.lamports(), FROM_AFTER_BALANCE);
    assert_eq!(to_acc.lamports(), TO_AFTER_BALANCE);

    assert_eq!(bank.get_balance(&from), from_acc.lamports());
    assert_eq!(bank.get_balance(&to), to_acc.lamports());

    // Balances
    assert_matches!(
        balances,
        TransactionBalancesSet {
            pre_balances: pre,
            post_balances: post,
        } => {
            assert_eq!(pre.len(), 1);
            assert_eq!(pre[0], [LAMPORTS_PER_SOL, 0, 1,]);

            assert_eq!(post.len(), 1);
            assert_eq!(post[0], [FROM_AFTER_BALANCE, TO_AFTER_BALANCE, 1,]);
        }
    );
}

#[test]
fn test_bank_system_allocate_instruction() {
    init_logger!();

    let (mut genesis_config, _) = create_genesis_config(u64::MAX);
    genesis_config.fee_rate_governor =
        FeeRateGovernor::new(LAMPORTS_PER_SIGNATURE, 0);

    let bank = Bank::new_for_tests(&genesis_config, None, None);

    const SPACE: u64 = 100;
    let rent: u64 = Rent::default().minimum_balance(SPACE as usize);

    let (tx, payer, account) =
        create_system_allocate_transaction(&bank, LAMPORTS_PER_SOL, SPACE);
    let (results, balances) = execute_transactions(&bank, vec![tx]);

    // Result
    let result = &results.execution_results[0];
    assert_matches!(result.details().unwrap().status, Ok(()));

    // Accounts
    let payer_acc = bank.get_account(&payer).unwrap();
    let recvr_acc = bank.get_account(&account).unwrap();

    assert_eq!(
        payer_acc.lamports(),
        LAMPORTS_PER_SOL - 2 * LAMPORTS_PER_SIGNATURE
    );
    assert_eq!(recvr_acc.lamports(), rent);
    assert_eq!(recvr_acc.data().len(), SPACE as usize);

    // Balances
    assert_matches!(
        balances,
        TransactionBalancesSet {
            pre_balances: pre,
            post_balances: post,
        } => {
            assert_eq!(pre.len(), 1);
            assert_eq!(pre[0], [1000000000, 1586880, 1,]);

            assert_eq!(post.len(), 1);
            assert_eq!(post[0], [999990000, 1586880, 1,]);
        }
    );
}

#[test]
fn test_bank_one_noop_instruction() {
    init_logger!();

    let (genesis_config, _) = create_genesis_config(u64::MAX);
    let bank = Bank::new_for_tests(&genesis_config, None, None);
    add_elf_program(&bank, &elfs::noop::ID);

    let tx = create_noop_transaction(&bank, bank.last_blockhash());
    bank.advance_slot();
    execute_and_check_results(&bank, tx);
}

#[test]
fn test_bank_expired_noop_instruction() {
    init_logger!();

    let (genesis_config, _) = create_genesis_config(u64::MAX);
    let bank = Bank::new_for_tests(&genesis_config, None, None);
    add_elf_program(&bank, &elfs::noop::ID);

    let tx = create_noop_transaction(&bank, Hash::new_unique());
    bank.advance_slot();

    let (results, _) = execute_transactions(&bank, vec![tx]);
    let result = &results.execution_results[0];
    assert!(!result.was_executed_successfully());
}

#[test]
fn test_bank_solx_instructions() {
    init_logger!();

    // 1. Init Bank and load solanax program
    let (mut genesis_config, _) = create_genesis_config(u64::MAX);
    genesis_config.fee_rate_governor =
        FeeRateGovernor::new(LAMPORTS_PER_SIGNATURE, 0);

    let bank = Bank::new_for_tests(&genesis_config, None, None);
    add_elf_program(&bank, &elfs::solanax::ID);

    // 2. Prepare Transaction and advance slot to activate solanax program
    let (tx, SolanaxPostAccounts { author: _, post }) =
        create_solx_send_post_transaction(&bank);
    let sig = *tx.signature();

    bank.advance_slot();

    // 3. Execute Transaction
    let (results, balances) = execute_transactions(&bank, vec![tx]);

    // 4. Check results
    let result = &results.execution_results[0];
    assert_matches!(result.details().unwrap().status, Ok(()));

    // Accounts
    let post_acc = bank.get_account(&post).unwrap();

    assert_eq!(post_acc.data().len(), 1180);
    assert_eq!(post_acc.owner(), &elfs::solanax::ID);

    // Balances
    assert_matches!(
        balances,
        TransactionBalancesSet {
            pre_balances: pre,
            post_balances: post,
        } => {
            assert_eq!(pre.len(), 1);
            assert_eq!(pre[0], [LAMPORTS_PER_SOL, 9103680, 1, 1141440]);

            assert_eq!(post.len(), 1);
            assert_eq!(post[0], [LAMPORTS_PER_SOL - 2 * LAMPORTS_PER_SIGNATURE , 9103680, 1, 1141440]);
        }
    );

    // Signature Status
    let sig_status = bank.get_signature_status(&sig);
    assert!(sig_status.is_some());
    assert_matches!(sig_status.as_ref().unwrap(), Ok(()));
}

fn execute_and_check_results(bank: &Bank, tx: SanitizedTransaction) {
    let results = execute_transactions(bank, vec![tx]).0.execution_results;
    let failures = results
        .iter()
        .filter(|r| !r.was_executed_successfully())
        .collect::<Vec<_>>();
    if !failures.is_empty() {
        panic!("Failures: {:#?}", failures);
    }
}

#[test]
fn test_bank_sysvars_get() {
    init_logger!();

    let (genesis_config, _) = create_genesis_config(u64::MAX);
    let bank = Bank::new_for_tests(&genesis_config, None, None);
    add_elf_program(&bank, &elfs::sysvars::ID);
    let tx = create_sysvars_get_transaction(&bank);
    bank.advance_slot();
    execute_and_check_results(&bank, tx);
}

#[test]
fn test_bank_sysvars_from_account() {
    init_logger!();

    let (genesis_config, _) = create_genesis_config(u64::MAX);
    let bank = Bank::new_for_tests(&genesis_config, None, None);
    add_elf_program(&bank, &elfs::sysvars::ID);
    let tx = create_sysvars_from_account_transaction(&bank);
    bank.advance_slot();
    execute_and_check_results(&bank, tx);
}
