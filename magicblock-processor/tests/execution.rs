use std::time::Duration;

use guinea::GuineaInstruction;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    native_token::LAMPORTS_PER_SOL,
};
use solana_signer::Signer;
use test_kit::ExecutionTestEnv;
use tokio::time::sleep;
const ACCOUNTS_COUNT: usize = 8;

#[tokio::test]
pub async fn test_transaction_with_return_data() {
    let env = ExecutionTestEnv::new();
    let accounts: Vec<_> = (0..ACCOUNTS_COUNT)
        .map(|_| env.create_account(LAMPORTS_PER_SOL, 128))
        .collect();
    let accounts = accounts
        .iter()
        .map(|a| AccountMeta::new_readonly(a.pubkey(), false))
        .collect();
    sleep(Duration::from_millis(500)).await;
    env.advance_slot();
    sleep(Duration::from_millis(500)).await;
    env.advance_slot();
    sleep(Duration::from_millis(500)).await;
    let ix = Instruction::new_with_bincode(
        guinea::ID,
        &GuineaInstruction::ComputeBalances,
        accounts,
    );
    let txn = env.build_transaction(&[ix]);
    let sig = txn.signatures[0];
    let result = env.execute_transaction(txn).await;
    assert!(
        result.is_ok(),
        "failed to execute compute balance transaction"
    );
    let meta = env.get_transaction(sig).expect( "transaction meta should have been written to the ledger after execution");
    let retdata = meta.return_data.expect(
        "transaction return data for compute balance should have been set",
    );
    assert_eq!(
        &retdata.data,
        &(ACCOUNTS_COUNT as u64 * LAMPORTS_PER_SOL).to_le_bytes(),
        "the total balance of accounts should have been in return data"
    );
}
