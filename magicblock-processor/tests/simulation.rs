use std::time::Duration;

use guinea::GuineaInstruction;
use magicblock_core::link::transactions::TransactionSimulationResult;
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

async fn simulate_transaction(
    env: &ExecutionTestEnv,
    metafn: fn(Pubkey, bool) -> AccountMeta,
    ix: GuineaInstruction,
) -> (TransactionSimulationResult, Signature, Vec<Pubkey>) {
    let accounts: Vec<_> = (0..ACCOUNTS_COUNT)
        .map(|_| {
            env.create_account_with_config(LAMPORTS_PER_SOL, 128, guinea::ID)
        })
        .collect();
    let accounts: Vec<_> =
        accounts.iter().map(|a| metafn(a.pubkey(), false)).collect();
    let pubkeys = accounts.iter().map(|m| m.pubkey).collect();
    env.advance_slot();
    let ix = Instruction::new_with_bincode(guinea::ID, &ix, accounts);
    let txn = env.build_transaction(&[ix]);
    let sig = txn.signatures[0];
    let result = env.simulate_transaction(txn).await;
    env.advance_slot();
    (result, sig, pubkeys)
}

#[tokio::test]
pub async fn test_absent_simulation_side_effects() {
    let env = ExecutionTestEnv::new();
    let (_, sig, pubkeys) = simulate_transaction(
        &env,
        AccountMeta::new,
        GuineaInstruction::WriteByteToData(42),
    )
    .await;
    let status = env
        .dispatch
        .transaction_status
        .recv_timeout(Duration::from_millis(200));
    assert!(
        env.dispatch.account_update.try_recv().is_err(),
        "transaction simulation should not have triggered account update notification"
    );
    assert!(
        status.is_err(),
        "transaction simulation should not have triggered signature status update"
    );
    let transaction = env.get_transaction(sig);
    assert!(
        transaction.is_none(),
        "simulated transaction should not have been persisted to the ledger"
    );
    for pubkey in &pubkeys {
        let account = env.accountsdb.get_account(pubkey).unwrap();
        assert!(
            account.data().first().map(|&b| b != 42).unwrap_or(true),
            "transaction simulation should not have modified account's state in the database"
         );
    }
}

#[tokio::test]
pub async fn test_simulation_logs() {
    let env = ExecutionTestEnv::new();
    let (result, _, _) = simulate_transaction(
        &env,
        AccountMeta::new_readonly,
        GuineaInstruction::PrintSizes,
    )
    .await;
    assert!(
        result.result.is_ok(),
        "failed to simulate print sizes transaction"
    );

    assert!(result.logs.unwrap().len() > ACCOUNTS_COUNT + 1,
        "print transaction should produce number of logs more than there're accounts in transaction"
    );

    assert!(
        result.inner_instructions.is_some(),
        "transaction simulation should always run with CPI recordings enabled"
    );
}

#[tokio::test]
pub async fn test_simulation_return_data() {
    let env = ExecutionTestEnv::new();
    let (result, _, _) = simulate_transaction(
        &env,
        AccountMeta::new_readonly,
        GuineaInstruction::ComputeBalances,
    )
    .await;
    assert!(
        result.result.is_ok(),
        "failed to simulate compute balance transaction"
    );
    let retdata = result.return_data.expect(
        "transaction simulation should run with return data support enabled",
    ).data;
    assert_eq!(
        &retdata,
        &(ACCOUNTS_COUNT as u64 * LAMPORTS_PER_SOL).to_le_bytes(),
        "the total balance of accounts should have been placed in return data"
    );
}
