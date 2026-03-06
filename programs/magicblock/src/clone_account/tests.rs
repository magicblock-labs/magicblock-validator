//! Tests for clone account instructions.

use std::collections::HashMap;

use magicblock_magic_program_api::instruction::{
    AccountCloneFields, MagicBlockInstruction,
};
use solana_account::{AccountSharedData, ReadableAccount};
use solana_instruction::{error::InstructionError, AccountMeta};
use test_kit::init_logger;

use super::*;
use crate::{
    errors::MagicBlockProgramError,
    instruction_utils::InstructionUtils,
    test_utils::{
        ensure_started_validator, process_instruction, AUTHORITY_BALANCE,
    },
};

fn clone_fields(lamports: u64, remote_slot: u64) -> AccountCloneFields {
    AccountCloneFields {
        lamports,
        remote_slot,
        ..Default::default()
    }
}

fn setup_with_account(
    pubkey: Pubkey,
    lamports: u64,
    remote_slot: u64,
) -> HashMap<Pubkey, AccountSharedData> {
    let mut account = AccountSharedData::new(lamports, 0, &pubkey);
    account.set_remote_slot(remote_slot);
    let mut map = HashMap::new();
    map.insert(pubkey, account);
    ensure_started_validator(&mut map);
    map
}

// Build transaction_accounts in instruction order (matching ix.accounts)
fn tx_accounts(
    mut account_data: HashMap<Pubkey, AccountSharedData>,
    ix_accounts: &[AccountMeta],
) -> Vec<(Pubkey, AccountSharedData)> {
    ix_accounts
        .iter()
        .flat_map(|acc| {
            account_data
                .remove(&acc.pubkey)
                .map(|shared_data| (acc.pubkey, shared_data))
        })
        .collect()
}

// -----------------
// CloneAccount
// -----------------

#[test]
fn test_rejects_wrong_signer_pubkey() {
    init_logger!();
    let pubkey = Pubkey::new_unique();
    let wrong_signer = Pubkey::new_unique();
    let mut accounts = setup_with_account(pubkey, 100, 0);
    // Add wrong signer account
    accounts
        .insert(wrong_signer, AccountSharedData::new(1000, 0, &wrong_signer));

    // Build instruction with WRONG pubkey as signer (not validator authority)
    let ix = solana_instruction::Instruction::new_with_bincode(
        crate::id(),
        &MagicBlockInstruction::CloneAccount {
            pubkey,
            data: vec![],
            fields: clone_fields(200, 0),
        },
        vec![
            AccountMeta::new(wrong_signer, true), // wrong signer!
            AccountMeta::new(pubkey, false),
        ],
    );

    process_instruction(
        &ix.data,
        tx_accounts(accounts, &ix.accounts),
        ix.accounts,
        Err(InstructionError::MissingRequiredSignature),
    );
}

#[test]
fn test_clone_account_basic() {
    init_logger!();
    let pubkey = Pubkey::new_unique();
    let accounts = setup_with_account(pubkey, 100, 0);

    let fields = AccountCloneFields {
        lamports: 500,
        owner: Pubkey::from([1; 32]),
        executable: true,
        delegated: true,
        confined: true,
        remote_slot: 42,
    };
    let ix = InstructionUtils::clone_account_instruction(
        pubkey,
        vec![1, 2, 3],
        fields,
    );

    let mut result = process_instruction(
        &ix.data,
        tx_accounts(accounts, &ix.accounts),
        ix.accounts,
        Ok(()),
    );
    result.drain(0..1); // skip authority
    let account = result.drain(0..1).next().unwrap();

    assert_eq!(account.lamports(), 500);
    assert_eq!(account.owner(), &Pubkey::from([1; 32]));
    assert!(account.executable());
    assert!(account.delegated());
    assert!(account.confined());
    assert_eq!(account.remote_slot(), 42);
    assert_eq!(account.data(), &[1, 2, 3]);
}

#[test]
fn test_clone_account_rejects_stale_slot() {
    init_logger!();
    let pubkey = Pubkey::new_unique();
    let accounts = setup_with_account(pubkey, 100, 100);

    let ix = InstructionUtils::clone_account_instruction(
        pubkey,
        vec![],
        clone_fields(200, 50), // slot 50 < current 100
    );

    process_instruction(
        &ix.data,
        tx_accounts(accounts, &ix.accounts),
        ix.accounts,
        Err(MagicBlockProgramError::OutOfOrderUpdate.into()),
    );
}

#[test]
fn test_clone_account_rejects_delegated_account() {
    init_logger!();
    let pubkey = Pubkey::new_unique();
    let mut account = AccountSharedData::new(100, 0, &pubkey);
    account.set_delegated(true);
    let mut accounts = HashMap::new();
    accounts.insert(pubkey, account);
    ensure_started_validator(&mut accounts);

    let ix = InstructionUtils::clone_account_instruction(
        pubkey,
        vec![],
        clone_fields(200, 0),
    );

    process_instruction(
        &ix.data,
        tx_accounts(accounts, &ix.accounts),
        ix.accounts,
        Err(MagicBlockProgramError::AccountIsDelegated.into()),
    );
}

#[test]
fn test_clone_account_rejects_ephemeral_account() {
    init_logger!();
    let pubkey = Pubkey::new_unique();
    let mut account = AccountSharedData::new(100, 0, &pubkey);
    account.set_ephemeral(true);
    let mut accounts = HashMap::new();
    accounts.insert(pubkey, account);
    ensure_started_validator(&mut accounts);

    let ix = InstructionUtils::clone_account_instruction(
        pubkey,
        vec![],
        clone_fields(200, 0),
    );

    process_instruction(
        &ix.data,
        tx_accounts(accounts, &ix.accounts),
        ix.accounts,
        Err(MagicBlockProgramError::AccountIsEphemeral.into()),
    );
}

#[test]
fn test_clone_account_adjusts_authority_lamports() {
    init_logger!();
    let pubkey = Pubkey::new_unique();
    let accounts = setup_with_account(pubkey, 100, 0);

    let ix = InstructionUtils::clone_account_instruction(
        pubkey,
        vec![],
        clone_fields(300, 0),
    );
    let mut result = process_instruction(
        &ix.data,
        tx_accounts(accounts, &ix.accounts),
        ix.accounts,
        Ok(()),
    );

    let authority = result.drain(0..1).next().unwrap();
    assert_eq!(authority.lamports(), AUTHORITY_BALANCE - 200); // 300 - 100 delta
}

// -----------------
// CloneAccountInit + Continue
// -----------------

#[test]
fn test_clone_init_allocates_buffer() {
    init_logger!();
    let pubkey = Pubkey::new_unique();
    let accounts = setup_with_account(pubkey, 0, 0);

    let ix = InstructionUtils::clone_account_init_instruction(
        pubkey,
        10,
        vec![1, 2, 3, 4],
        clone_fields(1000, 50),
    );
    let mut result = process_instruction(
        &ix.data,
        tx_accounts(accounts, &ix.accounts),
        ix.accounts,
        Ok(()),
    );

    result.drain(0..1); // authority
    let account = result.drain(0..1).next().unwrap();
    assert_eq!(account.data(), &[1, 2, 3, 4, 0, 0, 0, 0, 0, 0]);
    assert_eq!(account.lamports(), 1000);
    assert!(is_pending_clone(&pubkey));
}

#[test]
fn test_clone_continue_writes_at_offset() {
    init_logger!();
    let pubkey = Pubkey::new_unique();
    // Setup account with 10 bytes (simulating post-init state)
    let mut account = AccountSharedData::new(1000, 10, &pubkey);
    account.set_remote_slot(50);
    let mut accounts = HashMap::new();
    accounts.insert(pubkey, account);
    ensure_started_validator(&mut accounts);

    add_pending_clone(pubkey);

    let ix = InstructionUtils::clone_account_continue_instruction(
        pubkey,
        4,
        vec![5, 6, 7],
        false,
    );
    let mut result = process_instruction(
        &ix.data,
        tx_accounts(accounts, &ix.accounts),
        ix.accounts,
        Ok(()),
    );

    result.drain(0..1);
    let account = result.drain(0..1).next().unwrap();
    assert_eq!(&account.data()[4..7], &[5, 6, 7]);
    assert!(is_pending_clone(&pubkey)); // not removed when is_last=false
}

#[test]
fn test_clone_continue_completes_clone() {
    init_logger!();
    let pubkey = Pubkey::new_unique();
    let mut account = AccountSharedData::new(1000, 10, &pubkey);
    account.set_remote_slot(50);
    let mut accounts = HashMap::new();
    accounts.insert(pubkey, account);
    ensure_started_validator(&mut accounts);

    add_pending_clone(pubkey);

    let ix = InstructionUtils::clone_account_continue_instruction(
        pubkey,
        7,
        vec![8, 9, 10],
        true,
    );
    let mut result = process_instruction(
        &ix.data,
        tx_accounts(accounts, &ix.accounts),
        ix.accounts,
        Ok(()),
    );

    result.drain(0..1);
    let account = result.drain(0..1).next().unwrap();
    assert_eq!(&account.data()[7..10], &[8, 9, 10]);
    assert!(!is_pending_clone(&pubkey)); // removed when is_last=true
}

#[test]
fn test_clone_init_rejects_double_init() {
    init_logger!();
    let pubkey = Pubkey::new_unique();
    let accounts = setup_with_account(pubkey, 0, 0);

    add_pending_clone(pubkey);

    let ix = InstructionUtils::clone_account_init_instruction(
        pubkey,
        10,
        vec![],
        clone_fields(100, 0),
    );
    process_instruction(
        &ix.data,
        tx_accounts(accounts, &ix.accounts),
        ix.accounts,
        Err(MagicBlockProgramError::CloneAlreadyPending.into()),
    );
}

#[test]
fn test_clone_init_rejects_oversized_initial_data() {
    init_logger!();
    let pubkey = Pubkey::new_unique();
    let accounts = setup_with_account(pubkey, 0, 0);

    let ix = InstructionUtils::clone_account_init_instruction(
        pubkey,
        5,
        vec![1, 2, 3, 4, 5, 6],
        clone_fields(100, 0),
    );
    process_instruction(
        &ix.data,
        tx_accounts(accounts, &ix.accounts),
        ix.accounts,
        Err(InstructionError::InvalidArgument),
    );
}

#[test]
fn test_clone_continue_rejects_without_init() {
    init_logger!();
    let pubkey = Pubkey::new_unique();
    let accounts = setup_with_account(pubkey, 0, 0);

    let ix = InstructionUtils::clone_account_continue_instruction(
        pubkey,
        0,
        vec![1],
        true,
    );
    process_instruction(
        &ix.data,
        tx_accounts(accounts, &ix.accounts),
        ix.accounts,
        Err(MagicBlockProgramError::NoPendingClone.into()),
    );
}

#[test]
fn test_clone_continue_rejects_offset_overflow() {
    init_logger!();
    let pubkey = Pubkey::new_unique();
    let accounts = setup_with_account(pubkey, 0, 0);

    add_pending_clone(pubkey);
    // Account has 0 data bytes, but we try to write at offset 5
    let ix = InstructionUtils::clone_account_continue_instruction(
        pubkey,
        5,
        vec![1],
        true,
    );
    process_instruction(
        &ix.data,
        tx_accounts(accounts, &ix.accounts),
        ix.accounts,
        Err(InstructionError::InvalidArgument),
    );
}

// -----------------
// CleanupPartialClone
// -----------------

#[test]
fn test_cleanup_only_works_for_pending_clones() {
    init_logger!();
    let pubkey = Pubkey::new_unique();
    let accounts = setup_with_account(pubkey, 100, 0);

    // Not in pending clones
    let ix = InstructionUtils::cleanup_partial_clone_instruction(pubkey);
    process_instruction(
        &ix.data,
        tx_accounts(accounts, &ix.accounts),
        ix.accounts,
        Err(MagicBlockProgramError::NoPendingClone.into()),
    );
}

#[test]
fn test_cleanup_resets_account_and_returns_lamports() {
    init_logger!();
    let pubkey = Pubkey::new_unique();
    let accounts = setup_with_account(pubkey, 500, 0);

    add_pending_clone(pubkey);

    let ix = InstructionUtils::cleanup_partial_clone_instruction(pubkey);
    let mut result = process_instruction(
        &ix.data,
        tx_accounts(accounts, &ix.accounts),
        ix.accounts,
        Ok(()),
    );

    let authority = result.drain(0..1).next().unwrap();
    assert_eq!(authority.lamports(), AUTHORITY_BALANCE + 500); // got lamports back

    let account = result.drain(0..1).next().unwrap();
    assert_eq!(account.lamports(), 0);
    assert!(account.ephemeral()); // marked for removal

    assert!(!is_pending_clone(&pubkey));
}
