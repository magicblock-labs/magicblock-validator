use std::{collections::HashSet, time::Duration};

use guinea::GuineaInstruction;
use solana_account::{AccountSharedData, ReadableAccount, WritableAccount};
use solana_keypair::Keypair;
use solana_loader_v4_interface::{
    instruction::LoaderV4Instruction,
    state::{LoaderV4State, LoaderV4Status},
};
use solana_program::{
    instruction::{AccountMeta, Instruction},
    native_token::LAMPORTS_PER_SOL,
    rent::Rent,
};
use solana_program_runtime::solana_sbpf::ebpf;
use solana_pubkey::Pubkey;
use solana_sdk_ids::loader_v4;
use solana_signature::Signature;
use test_kit::{ExecutionTestEnv, Signer};

const ACCOUNTS_COUNT: usize = 8;
const TIMEOUT: Duration = Duration::from_millis(200);

/// Helper to execute a standard "Guinea" transaction on `ACCOUNTS_COUNT` new accounts.
async fn execute_guinea(
    env: &ExecutionTestEnv,
    ix: GuineaInstruction,
    is_writable: bool,
) -> (Signature, Vec<Pubkey>) {
    let accounts: Vec<_> = (0..ACCOUNTS_COUNT)
        .map(|_| {
            env.create_account_with_config(LAMPORTS_PER_SOL, 128, guinea::ID)
        })
        .collect();

    let metas = accounts
        .iter()
        .map(|a| {
            if is_writable {
                AccountMeta::new(a.pubkey(), false)
            } else {
                AccountMeta::new_readonly(a.pubkey(), false)
            }
        })
        .collect();

    env.advance_slot();
    let ix = Instruction::new_with_bincode(guinea::ID, &ix, metas);
    let txn = env.build_transaction(&[ix]);
    let sig = txn.signatures[0];

    assert!(
        env.execute_transaction(txn).await.is_ok(),
        "Transaction execution failed"
    );
    env.advance_slot();

    let pubkeys = accounts.iter().map(|a| a.pubkey()).collect();
    (sig, pubkeys)
}

#[tokio::test]
async fn test_transaction_with_return_data() {
    let env = ExecutionTestEnv::new();
    let (sig, _) =
        execute_guinea(&env, GuineaInstruction::ComputeBalances, false).await;

    let meta = env.get_transaction(sig).expect("Transaction not in ledger");
    let ret_data = meta.return_data.expect("Return data missing");

    let expected = (ACCOUNTS_COUNT as u64 * LAMPORTS_PER_SOL).to_le_bytes();
    assert_eq!(ret_data.data, expected, "Incorrect return data balance");
}

#[tokio::test]
async fn test_transaction_status_update() {
    let env = ExecutionTestEnv::new();
    let (sig, _) =
        execute_guinea(&env, GuineaInstruction::PrintSizes, false).await;

    let status = env
        .dispatch
        .transaction_status
        .recv_timeout(TIMEOUT)
        .expect("Status update missing");

    assert_eq!(status.txn.signatures()[0], sig);
    let logs = status.meta.log_messages.as_ref().expect("Logs missing");
    assert!(logs.len() > ACCOUNTS_COUNT, "Insufficient logs produced");
}

#[tokio::test]
async fn test_transaction_modifies_accounts() {
    let env = ExecutionTestEnv::new();
    let (_, accounts) =
        execute_guinea(&env, GuineaInstruction::WriteByteToData(42), true)
            .await;

    // 1. Verify DB state modifications
    let status = env
        .dispatch
        .transaction_status
        .recv_timeout(TIMEOUT)
        .expect("Status update missing");

    // Skip fee payer, check the guinea accounts
    let account_keys: Vec<_> = status
        .txn
        .message()
        .account_keys()
        .iter()
        .copied()
        .collect();
    for pubkey in account_keys.iter().skip(1).take(ACCOUNTS_COUNT) {
        let account = env.get_account(*pubkey);
        assert_eq!(account.data()[0], 42, "Account data mismatch");
    }

    // 2. Verify update notifications
    let mut updated_accounts = HashSet::new();
    while let Ok(update) = env.dispatch.account_update.try_recv() {
        updated_accounts.insert(update.account.pubkey);
    }

    let expected_accounts: HashSet<_> = HashSet::from_iter(accounts);
    assert!(
        updated_accounts.is_superset(&expected_accounts),
        "Missing account update notifications"
    );
}

#[tokio::test]
async fn test_loader_v4_deploy_accepts_stripped_sbpf_v3_program() {
    let env = ExecutionTestEnv::new();
    let authority = Keypair::new();
    env.fund_account(authority.pubkey(), LAMPORTS_PER_SOL);

    let program = Keypair::new();
    let program_data = minimal_stripped_sbpf_v3_elf();
    let account_data =
        loader_v4_account_data(&authority.pubkey(), &program_data);
    let mut account = AccountSharedData::new(
        Rent::default().minimum_balance(account_data.len()),
        account_data.len(),
        &loader_v4::ID,
    );
    account.set_data_from_slice(&account_data);
    account.set_executable(true);
    account.set_delegated(true);
    env.accountsdb
        .insert_account(&program.pubkey(), &account)
        .unwrap();

    let deploy_ix = Instruction {
        program_id: loader_v4::ID,
        accounts: vec![
            AccountMeta::new(program.pubkey(), false),
            AccountMeta::new_readonly(authority.pubkey(), true),
        ],
        data: bincode::serialize(&LoaderV4Instruction::Deploy).unwrap(),
    };

    env.advance_slot();
    let txn = env.build_transaction_with_signers(&[deploy_ix], &[&authority]);
    env.execute_transaction(txn)
        .await
        .expect("LoaderV4 deploy should accept the synthetic sBPF v3 ELF");

    let program_account = env.get_account(program.pubkey());
    let loader_state =
        solana_loader_v4_program::get_state(program_account.data()).unwrap();
    assert_eq!(loader_state.status, LoaderV4Status::Deployed);
}

fn loader_v4_account_data(authority: &Pubkey, program_data: &[u8]) -> Vec<u8> {
    let state = LoaderV4State {
        slot: 0,
        authority_address_or_next_version: *authority,
        status: LoaderV4Status::Retracted,
    };
    let header = loader_v4_state_bytes(&state);
    let mut data = Vec::with_capacity(header.len() + program_data.len());
    data.extend_from_slice(header);
    data.extend_from_slice(program_data);
    data
}

fn loader_v4_state_bytes(state: &LoaderV4State) -> &[u8] {
    // Match the loader-v4 account header layout used by production code.
    unsafe {
        std::slice::from_raw_parts(
            (state as *const LoaderV4State) as *const u8,
            LoaderV4State::program_data_offset(),
        )
    }
}

fn minimal_stripped_sbpf_v3_elf() -> Vec<u8> {
    const ELF_HEADER_LEN: usize = 64;
    const PROGRAM_HEADER_LEN: usize = 56;
    const ET_DYN: u16 = 3;
    const EM_BPF: u16 = 247;
    const EV_CURRENT: u32 = 1;
    const PT_LOAD: u32 = 1;
    const PF_X: u32 = 1;
    const PF_R: u32 = 4;

    let rodata_offset = ELF_HEADER_LEN + PROGRAM_HEADER_LEN * 3;
    let rodata_len = ebpf::INSN_SIZE as u64;
    let text_offset = rodata_offset + rodata_len as usize;
    let text_len = ebpf::INSN_SIZE as u64;
    let mut bytes = Vec::with_capacity(text_offset + text_len as usize);

    bytes.extend_from_slice(&[
        0x7f, b'E', b'L', b'F', 2, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ]);
    push_u16(&mut bytes, ET_DYN);
    push_u16(&mut bytes, EM_BPF);
    push_u32(&mut bytes, EV_CURRENT);
    push_u64(&mut bytes, ebpf::MM_BYTECODE_START);
    push_u64(&mut bytes, ELF_HEADER_LEN as u64);
    push_u64(&mut bytes, 0);
    push_u32(&mut bytes, 3);
    push_u16(&mut bytes, ELF_HEADER_LEN as u16);
    push_u16(&mut bytes, PROGRAM_HEADER_LEN as u16);
    push_u16(&mut bytes, 3);
    push_u16(&mut bytes, 0);
    push_u16(&mut bytes, 0);
    push_u16(&mut bytes, 0);
    assert_eq!(bytes.len(), ELF_HEADER_LEN);

    push_program_header(
        &mut bytes,
        PT_LOAD,
        PF_R,
        rodata_offset as u64,
        ebpf::MM_RODATA_START,
        rodata_len,
    );
    push_program_header(
        &mut bytes,
        PT_LOAD,
        PF_X,
        text_offset as u64,
        ebpf::MM_BYTECODE_START,
        text_len,
    );
    push_program_header(&mut bytes, 0, 0, 0, 0, 0);
    assert_eq!(bytes.len(), rodata_offset);

    bytes.extend_from_slice(b"fake-ro\0");
    assert_eq!(bytes.len(), text_offset);
    bytes.extend_from_slice(&[ebpf::EXIT, 0, 0, 0, 0, 0, 0, 0]);
    bytes
}

fn push_program_header(
    bytes: &mut Vec<u8>,
    p_type: u32,
    p_flags: u32,
    p_offset: u64,
    p_vaddr: u64,
    p_filesz: u64,
) {
    push_u32(bytes, p_type);
    push_u32(bytes, p_flags);
    push_u64(bytes, p_offset);
    push_u64(bytes, p_vaddr);
    push_u64(bytes, p_vaddr);
    push_u64(bytes, p_filesz);
    push_u64(bytes, p_filesz);
    push_u64(bytes, ebpf::INSN_SIZE as u64);
}

fn push_u16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn push_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_le_bytes());
}
