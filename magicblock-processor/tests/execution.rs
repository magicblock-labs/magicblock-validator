use std::time::Duration;

use guinea::GuineaInstruction;
use magicblock_core::link::transactions::TransactionResult;
use solana_account::ReadableAccount;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    native_token::LAMPORTS_PER_SOL,
};
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_signer::Signer;
use test_kit::ExecutionTestEnv;
const ACCOUNTS_COUNT: usize = 8;

async fn execute_transaction(
    env: &ExecutionTestEnv,
    metafn: fn(Pubkey, bool) -> AccountMeta,
    ix: GuineaInstruction,
) -> (TransactionResult, Signature) {
    let accounts: Vec<_> = (0..ACCOUNTS_COUNT)
        .map(|_| {
            env.create_account_with_config(LAMPORTS_PER_SOL, 128, guinea::ID)
        })
        .collect();
    let accounts = accounts.iter().map(|a| metafn(a.pubkey(), false)).collect();
    env.advance_slot();
    let ix = Instruction::new_with_bincode(guinea::ID, &ix, accounts);
    let txn = env.build_transaction(&[ix]);
    let sig = txn.signatures[0];
    let result = env.execute_transaction(txn).await;
    env.advance_slot();
    (result, sig)
}

#[tokio::test]
pub async fn test_transaction_with_return_data() {
    let env = ExecutionTestEnv::new();
    let (result, sig) = execute_transaction(
        &env,
        AccountMeta::new_readonly,
        GuineaInstruction::ComputeBalances,
    )
    .await;
    assert!(
        result.is_ok(),
        "failed to execute compute balance transaction"
    );
    let meta = env.get_transaction(sig).expect(
        "transaction meta should have been written to the ledger after execution"
    );
    let retdata = meta.return_data.expect(
        "transaction return data for compute balance should have been set",
    );
    assert_eq!(
        &retdata.data,
        &(ACCOUNTS_COUNT as u64 * LAMPORTS_PER_SOL).to_le_bytes(),
        "the total balance of accounts should have been placed in return data"
    );
}

#[tokio::test]
pub async fn test_transaction_status_update() {
    let env = ExecutionTestEnv::new();
    let (result, sig) = execute_transaction(
        &env,
        AccountMeta::new_readonly,
        GuineaInstruction::PrintSizes,
    )
    .await;
    assert!(result.is_ok(), "failed to execute print sizes transaction");
    let status = env.dispatch
        .transaction_status
        .recv_timeout(Duration::from_millis(300))
        .expect("successful transaction status should be delivered immediately after execution");
    assert_eq!(
        status.signature, sig,
        "update signature should match with executed txn"
    );
    assert!(
        status.result.logs.is_some(),
        "print transaction should have produced some logs"
    );
    println!("{:?}", status.result.logs.as_ref().unwrap());
    assert!(status.result.logs.unwrap().len() > ACCOUNTS_COUNT + 1,
        "print transaction should produce number of logs more than there're accounts in transaction"
    );
}

#[tokio::test]
pub async fn test_transaction_modifies_accounts() {
    let env = ExecutionTestEnv::new();
    let (result, _) = execute_transaction(
        &env,
        AccountMeta::new,
        GuineaInstruction::WriteByteToData(42),
    )
    .await;
    assert!(result.is_ok(), "failed to execute write byte transaction");
    let status = env.dispatch
        .transaction_status
        .recv_timeout(Duration::from_millis(300))
        .expect("successful transaction status should be delivered immediately after execution");
    // iterate over transaction accounts except for the payer
    for acc in status.result.accounts.iter().skip(1).take(ACCOUNTS_COUNT) {
        let account = env
            .accountsdb
            .get_account(&acc)
            .expect("transaction account should be in database");
        assert_eq!(
            *account.data().first().unwrap(),
            42,
            "the first byte of all accounts should have been modified"
        )
    }
}
