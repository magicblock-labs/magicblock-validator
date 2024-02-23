mod utils;

use sleipnir_bank::bank::Bank;
use solana_sdk::genesis_config::create_genesis_config;
use utils::{add_elf_program, elfs};

use crate::utils::init_logger;

#[test]
fn test_bank_one_system_instruction() {
    init_logger();

    let (genesis_config, _) = create_genesis_config(u64::MAX);
    let bank = Bank::new_for_tests(&genesis_config);

    let txs = utils::create_system_transfer_transactions(&bank, 1);
    utils::execute_transactions(&bank, txs);
}

#[test]
fn test_bank_one_noop_instruction() {
    init_logger();

    let (genesis_config, _) = create_genesis_config(u64::MAX);
    let mut bank = Bank::new_for_tests(&genesis_config);
    add_elf_program(&bank, &elfs::noop::ID);

    let tx = utils::create_noop_transaction(&bank);
    bank.advance_slot();
    utils::execute_transactions(&bank, vec![tx]);
}

#[test]
fn test_bank_solx_instructions() {
    init_logger();

    let (genesis_config, _) = create_genesis_config(u64::MAX);
    let mut bank = Bank::new_for_tests(&genesis_config);
    add_elf_program(&bank, &elfs::solanax::ID);
    let tx = utils::create_solx_send_post_transaction(&bank);
    bank.advance_slot();
    utils::execute_transactions(&bank, vec![tx]);
}

#[test]
fn test_bank_sysvars_get() {
    init_logger();

    let (genesis_config, _) = create_genesis_config(u64::MAX);
    let mut bank = Bank::new_for_tests(&genesis_config);
    add_elf_program(&bank, &elfs::sysvars::ID);
    let tx = utils::create_sysvars_get_transaction(&bank);
    bank.advance_slot();
    utils::execute_transactions(&bank, vec![tx]);
}
