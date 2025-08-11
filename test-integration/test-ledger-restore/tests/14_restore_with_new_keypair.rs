use cleanass::assert_eq;
use solana_rpc_client::rpc_client::RpcClient;
use std::{path::Path, process::Child};

use integration_test_tools::{
    expect, loaded_accounts::LoadedAccounts, tmpdir::resolve_tmp_dir,
    validator::cleanup,
};
use solana_sdk::{
    account::Account, bpf_loader_upgradeable, instruction::Instruction,
    native_token::LAMPORTS_PER_SOL, pubkey::Pubkey, signature::Keypair,
    signer::Signer, transaction::Transaction,
};
use test_ledger_restore::{
    kill_validator, setup_validator_with_local_remote, wait_for_ledger_persist,
    TMP_DIR_LEDGER,
};

const MEMO_PROGRAM_PK: Pubkey = Pubkey::new_from_array([
    5, 74, 83, 90, 153, 41, 33, 6, 77, 36, 232, 113, 96, 218, 56, 124, 124, 53,
    181, 221, 188, 146, 187, 129, 228, 31, 168, 64, 65, 5, 68, 141,
]);

// In this test we ensure that restoring from a ledger with a new validator
// authority works.
// This assumes a solana-test-validator is running on port 7799.

#[test]
fn restore_ledger_with_new_validator_authority() {
    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);

    // Write a transaction that clones the memo program
    let (mut validator, _) = write(&ledger_path);
    kill_validator(&mut validator, 8899);

    // Read the ledger and verify that the memo program is cloned
    let mut validator = read(&ledger_path);
    kill_validator(&mut validator, 8899);
}

fn write(ledger_path: &Path) -> (Child, u64) {
    let loaded_chain_accounts =
        LoadedAccounts::new_with_new_validator_authority();
    // Airdrop to the new validator authority
    let rpc_client = RpcClient::new("http://localhost:7799");
    rpc_client
        .request_airdrop(
            &loaded_chain_accounts.validator_authority(),
            10 * LAMPORTS_PER_SOL,
        )
        .unwrap();
    wait_for_airdrop(&rpc_client, &loaded_chain_accounts.validator_authority());

    let (_, mut validator, ctx) = setup_validator_with_local_remote(
        ledger_path,
        None,
        true,
        false,
        &loaded_chain_accounts,
    );

    let payer = Keypair::new();
    expect!(
        ctx.airdrop_chain(&payer.pubkey(), LAMPORTS_PER_SOL),
        validator
    );

    // This transaction will clone the memo program
    let memo_ix = Instruction::new_with_bytes(
        MEMO_PROGRAM_PK,
        &[
            0x39, 0x34, 0x32, 0x32, 0x38, 0x30, 0x37, 0x2e, 0x35, 0x34, 0x30,
            0x30, 0x30, 0x32,
        ],
        vec![],
    );
    let mut tx = Transaction::new_with_payer(&[memo_ix], Some(&payer.pubkey()));
    expect!(
        ctx.send_and_confirm_transaction_ephem(&mut tx, &[&payer]),
        validator
    );

    let account = expect!(
        ctx.try_ephem_client()
            .map_err(|e| anyhow::anyhow!("{}", e))
            .and_then(|client| client
                .get_account(&MEMO_PROGRAM_PK)
                .map_err(|e| anyhow::anyhow!("{}", e))),
        validator
    );
    let Account {
        owner, executable, ..
    } = account;
    assert_eq!(owner, bpf_loader_upgradeable::ID, cleanup(&mut validator));
    assert_eq!(executable, true, cleanup(&mut validator));

    let slot = wait_for_ledger_persist(&mut validator);

    (validator, slot)
}

fn read(ledger_path: &Path) -> Child {
    let loaded_chain_accounts =
        LoadedAccounts::new_with_new_validator_authority();
    // Airdrop to the new validator authority
    let rpc_client = RpcClient::new("http://localhost:7799");
    rpc_client
        .request_airdrop(
            &loaded_chain_accounts.validator_authority(),
            10 * LAMPORTS_PER_SOL,
        )
        .unwrap();
    wait_for_airdrop(&rpc_client, &loaded_chain_accounts.validator_authority());

    let (_, mut validator, ctx) = setup_validator_with_local_remote(
        ledger_path,
        None,
        false,
        true,
        &loaded_chain_accounts,
    );

    let account = expect!(
        ctx.try_ephem_client()
            .map_err(|e| anyhow::anyhow!("{}", e))
            .and_then(|client| client
                .get_account(&MEMO_PROGRAM_PK)
                .map_err(|e| anyhow::anyhow!("{}", e))),
        validator
    );
    let Account {
        owner, executable, ..
    } = account;
    assert_eq!(owner, bpf_loader_upgradeable::ID, cleanup(&mut validator));
    assert_eq!(executable, true, cleanup(&mut validator));

    validator
}

fn wait_for_airdrop(rpc_client: &RpcClient, pubkey: &Pubkey) {
    loop {
        let res = rpc_client.get_balance(pubkey).unwrap();
        if res > 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(400));
    }
}
