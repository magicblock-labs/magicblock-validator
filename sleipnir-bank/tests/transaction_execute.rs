#![cfg(feature = "dev-context-only-utils")]

use log::*;
use sleipnir_bank::bank::Bank;
use sleipnir_bank::bank_dev_utils::elfs;
use sleipnir_bank::bank_dev_utils::elfs::add_elf_program;
use sleipnir_bank::bank_dev_utils::transactions::{
    create_noop_transaction, create_solx_send_post_transaction,
    create_system_transfer_transactions, create_sysvars_from_account_transaction,
    create_sysvars_get_transaction, execute_transactions,
};
use solana_sdk::account::ReadableAccount;
use solana_sdk::genesis_config::create_genesis_config;
use test_tools_core::init_logger;

#[test]
fn test_bank_one_system_instruction() {
    init_logger!();

    let (genesis_config, _) = create_genesis_config(u64::MAX);
    let bank = Bank::new_for_tests(&genesis_config);

    let txs = create_system_transfer_transactions(&bank, 1);
    execute_transactions(&bank, txs);
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
    let (tx, post, payer) = create_solx_send_post_transaction(&bank);
    bank.advance_slot();
    execute_transactions(&bank, vec![tx]);

    let payer_acc = bank.get_account(&payer).unwrap();
    let post_acc = bank.get_account(&post).unwrap();
    assert_eq!(post_acc.data().len(), 1180);
    debug!("Payer account: {:#?}", payer_acc);
    debug!("Post account: {:#?}", post_acc);
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
