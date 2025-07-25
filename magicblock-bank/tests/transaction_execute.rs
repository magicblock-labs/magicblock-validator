#![cfg(feature = "dev-context-only-utils")]

use assert_matches::assert_matches;
use magicblock_bank::{
    bank::Bank,
    bank_dev_utils::{
        elfs::{self, add_elf_program},
        transactions::{
            create_noop_instruction, create_noop_transaction,
            create_solx_send_post_transaction,
            create_system_allocate_transaction,
            create_system_transfer_transaction,
            create_sysvars_from_account_transaction,
            create_sysvars_get_transaction, execute_transactions,
            SolanaxPostAccounts,
        },
    },
    genesis_utils::create_genesis_config_with_leader,
    transaction_results::TransactionBalancesSet,
    DEFAULT_LAMPORTS_PER_SIGNATURE,
};
use solana_sdk::{
    account::ReadableAccount,
    genesis_config::create_genesis_config,
    message::Message,
    native_token::LAMPORTS_PER_SOL,
    pubkey::Pubkey,
    rent::Rent,
    signature::Keypair,
    transaction::{SanitizedTransaction, Transaction},
};
use test_tools_core::init_logger;

#[test]
fn test_bank_system_transfer_instruction() {
    init_logger!();

    let genesis_config_info = create_genesis_config_with_leader(
        u64::MAX,
        &Pubkey::new_unique(),
        None,
    );
    let bank =
        Bank::new_for_tests(&genesis_config_info.genesis_config, None, None)
            .unwrap();

    let (tx, from, to) = create_system_transfer_transaction(
        &bank,
        LAMPORTS_PER_SOL,
        LAMPORTS_PER_SOL / 5,
    );
    let (results, balances) = execute_transactions(&bank, vec![tx]);

    const FROM_AFTER_BALANCE: u64 = LAMPORTS_PER_SOL
        - LAMPORTS_PER_SOL / 5
        - DEFAULT_LAMPORTS_PER_SIGNATURE;
    const TO_AFTER_BALANCE: u64 = LAMPORTS_PER_SOL / 5;

    // Result
    let result = &results[0];
    assert_matches!(result, Ok(_));

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

    const LAMPORTS_PER_SIGNATURE: u64 = 5000;

    let genesis_config_info = create_genesis_config_with_leader(
        u64::MAX,
        &Pubkey::new_unique(),
        Some(LAMPORTS_PER_SIGNATURE),
    );
    let bank =
        Bank::new_for_tests(&genesis_config_info.genesis_config, None, None)
            .unwrap();

    const SPACE: u64 = 100;
    let rent: u64 = Rent::default().minimum_balance(SPACE as usize);

    let (tx, payer, account) =
        create_system_allocate_transaction(&bank, LAMPORTS_PER_SOL, SPACE);
    let (results, balances) = execute_transactions(&bank, vec![tx]);

    // Result
    let result = &results[0];
    assert_matches!(result, Ok(_));

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
            assert_eq!(post[0], [1000000000 - 2 * LAMPORTS_PER_SIGNATURE, 1586880, 1,]);
        }
    );
}

#[test]
fn test_bank_one_noop_instruction() {
    init_logger!();

    let (genesis_config, _) = create_genesis_config(u64::MAX);
    let bank = Bank::new_for_tests(&genesis_config, None, None).unwrap();
    add_elf_program(&bank, &elfs::noop::ID);

    bank.advance_slot();
    let hash = bank.last_blockhash();
    let tx = create_noop_transaction(&bank, hash);
    execute_and_check_results(&bank, tx);
}

#[test]
fn test_bank_one_noop_instruction_0_fees_not_existing_feepayer() {
    init_logger!();

    let (genesis_config, _) = create_genesis_config(u64::MAX);
    let bank = Bank::new_for_tests(&genesis_config, None, None).unwrap();
    add_elf_program(&bank, &elfs::noop::ID);

    bank.advance_slot();
    let hash = bank.last_blockhash();
    let fee_payer = Keypair::new();
    let instruction = create_noop_instruction(
        &elfs::noop::id(),
        &[fee_payer.insecure_clone()],
    );
    let message = Message::new(&[instruction], None);
    let transaction = Transaction::new(&[fee_payer], message, hash);
    let tx = SanitizedTransaction::try_from_legacy_transaction(
        transaction,
        &Default::default(),
    )
    .unwrap();
    execute_and_check_results(&bank, tx);
}

#[test]
fn test_bank_expired_noop_instruction() {
    init_logger!();

    let (genesis_config, _) = create_genesis_config(u64::MAX);
    let bank = Bank::new_for_tests(&genesis_config, None, None).unwrap();
    add_elf_program(&bank, &elfs::noop::ID);

    let tx = create_noop_transaction(&bank, bank.last_blockhash());
    bank.advance_slot();

    let (results, _) = execute_transactions(&bank, vec![tx]);
    let result = &results[0];
    assert_matches!(result, Ok(_));
}

fn run_solx_instruction_test(lamports_per_signature: Option<u64>) {
    init_logger!();

    // 1. Init Bank and load solanax program
    let genesis_config_info = create_genesis_config_with_leader(
        u64::MAX,
        &Pubkey::new_unique(),
        lamports_per_signature,
    );
    let bank =
        Bank::new_for_tests(&genesis_config_info.genesis_config, None, None)
            .unwrap();
    add_elf_program(&bank, &elfs::solanax::ID);

    // 2. Prepare Transaction and advance slot to activate solanax program
    let (tx, SolanaxPostAccounts { author: _, post }) =
        create_solx_send_post_transaction(&bank);
    let sig = *tx.signature();

    bank.advance_slot();

    // 3. Execute Transaction
    let (results, balances) = execute_transactions(&bank, vec![tx]);

    // 4. Check results
    let result = &results[0];
    assert_matches!(result, Ok(_));

    // Accounts
    let post_acc = bank.get_account(&post).unwrap();

    assert_eq!(post_acc.data().len(), 1180);
    assert_eq!(post_acc.owner(), &elfs::solanax::ID);

    // Balances
    let expected_fee =
        lamports_per_signature.unwrap_or(DEFAULT_LAMPORTS_PER_SIGNATURE);
    assert_matches!(
        balances,
        TransactionBalancesSet {
            pre_balances: pre,
            post_balances: post,
        } => {
            assert_eq!(pre.len(), 1);
            assert_eq!(pre[0], [LAMPORTS_PER_SOL, 9103680, 1, 1141440]);

            assert_eq!(post.len(), 1);
            assert_eq!(post[0], [LAMPORTS_PER_SOL - 2 * expected_fee, 9103680, 1, 1141440]);
        }
    );

    // Signature Status
    let sig_status = bank.get_signature_status(&sig);
    assert!(sig_status.is_some());
    assert_matches!(sig_status.as_ref().unwrap(), Ok(()));
}

#[test]
fn test_bank_solx_instructions() {
    run_solx_instruction_test(None);
}

#[test]
fn test_bank_solx_instructions_with_fees() {
    run_solx_instruction_test(Some(5000));
}

fn execute_and_check_results(bank: &Bank, tx: SanitizedTransaction) {
    let (results, _) = execute_transactions(bank, vec![tx]);
    let failures = results.iter().filter(|r| r.is_err()).collect::<Vec<_>>();
    if !failures.is_empty() {
        panic!("Failures: {:#?}", failures);
    }
}

#[test]
fn test_bank_sysvars_get() {
    init_logger!();

    let (genesis_config, _) = create_genesis_config(u64::MAX);
    let bank = Bank::new_for_tests(&genesis_config, None, None).unwrap();
    add_elf_program(&bank, &elfs::sysvars::ID);
    let tx = create_sysvars_get_transaction(&bank);
    bank.advance_slot();
    execute_and_check_results(&bank, tx);
}

#[test]
fn test_bank_sysvars_from_account() {
    init_logger!();

    let (genesis_config, _) = create_genesis_config(u64::MAX);
    let bank = Bank::new_for_tests(&genesis_config, None, None).unwrap();
    add_elf_program(&bank, &elfs::sysvars::ID);
    let tx = create_sysvars_from_account_transaction(&bank);
    bank.advance_slot();
    execute_and_check_results(&bank, tx);
}
