#![cfg(feature = "dev-context-only-utils")]

use assert_matches::assert_matches;
use log::*;
use sleipnir_bank::bank::Bank;
use sleipnir_bank::bank_dev_utils::elfs;
use sleipnir_bank::bank_dev_utils::elfs::add_elf_program;
use sleipnir_bank::bank_dev_utils::transactions::{
    create_noop_transaction, create_solx_send_post_transaction, create_system_allocate_transaction,
    create_system_transfer_transaction, create_sysvars_from_account_transaction,
    create_sysvars_get_transaction, execute_transactions,
};
use sleipnir_bank::transaction_results::TransactionBalancesSet;
use sleipnir_bank::LAMPORTS_PER_SIGNATURE;
use solana_sdk::account::ReadableAccount;
use solana_sdk::genesis_config::create_genesis_config;
use solana_sdk::native_token::LAMPORTS_PER_SOL;
use solana_sdk::rent::Rent;
use test_tools_core::init_logger;

#[test]
fn test_bank_system_transfer_instruction() {
    init_logger!();

    let (genesis_config, _) = create_genesis_config(u64::MAX);
    let bank = Bank::new_for_tests(&genesis_config);

    let (tx, from, to) =
        create_system_transfer_transaction(&bank, LAMPORTS_PER_SOL, LAMPORTS_PER_SOL / 5);
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

    assert_eq!(from_acc.lamports(), FROM_AFTER_BALANCE,);
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
fn test_bank_system_acllocate_instruction() {
    init_logger!();

    let (genesis_config, _) = create_genesis_config(u64::MAX);
    let bank = Bank::new_for_tests(&genesis_config);

    const SPACE: u64 = 100;
    let rent: u64 = Rent::default().minimum_balance(SPACE as usize);

    let (tx, payer, account) = create_system_allocate_transaction(&bank, LAMPORTS_PER_SOL, SPACE);
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
    let bank = Bank::new_for_tests(&genesis_config);
    add_elf_program(&bank, &elfs::noop::ID);

    let tx = create_noop_transaction(&bank);
    bank.advance_slot();
    execute_transactions(&bank, vec![tx]);
}

#[test]
fn test_bank_solx_instructions() {
    init_logger!();

    let (genesis_config, _) = create_genesis_config(u64::MAX);
    let bank = Bank::new_for_tests(&genesis_config);
    add_elf_program(&bank, &elfs::solanax::ID);
    let (tx, payer, post) = create_solx_send_post_transaction(&bank);

    let sig = *tx.signature();
    bank.advance_slot();

    debug!("payer: {}", payer);
    debug!("post: {}", post);
    execute_transactions(&bank, vec![tx]);

    let payer_acc = bank.get_account(&payer);
    let post_acc = bank.get_account(&post);
    // assert_eq!(post_acc.data().len(), 1180);
    debug!("Payer account: {:#?}", payer_acc);
    debug!("Post account: {:#?}", post_acc);

    let sig_status = bank.get_signature_status(&sig);
    assert!(sig_status.is_some());
    assert_matches!(sig_status.as_ref().unwrap(), Ok(()));
}

#[test]
fn test_bank_sysvars_get() {
    init_logger!();

    let (genesis_config, _) = create_genesis_config(u64::MAX);
    let bank = Bank::new_for_tests(&genesis_config);
    add_elf_program(&bank, &elfs::sysvars::ID);
    let tx = create_sysvars_get_transaction(&bank);
    bank.advance_slot();
    execute_transactions(&bank, vec![tx]);
}

#[test]
fn test_bank_sysvars_from_account() {
    init_logger!();

    let (genesis_config, _) = create_genesis_config(u64::MAX);
    let bank = Bank::new_for_tests(&genesis_config);
    add_elf_program(&bank, &elfs::sysvars::ID);
    let tx = create_sysvars_from_account_transaction(&bank);
    bank.advance_slot();
    execute_transactions(&bank, vec![tx]);
}
