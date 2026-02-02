use guinea::GuineaInstruction;
use solana_account::{ReadableAccount, WritableAccount};
use solana_keypair::Keypair;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    native_token::LAMPORTS_PER_SOL,
};
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction_error::TransactionError;
use test_kit::ExecutionTestEnv;

fn transfer_ix(from: Pubkey, to: Pubkey, amount: u64) -> Instruction {
    Instruction::new_with_bincode(
        guinea::ID,
        &GuineaInstruction::Transfer(amount),
        vec![AccountMeta::new(from, true), AccountMeta::new(to, false)],
    )
}

fn setup_confined(env: &ExecutionTestEnv) -> Keypair {
    let acc = env.create_account_with_config(LAMPORTS_PER_SOL, 42, guinea::ID);
    let mut data = env.get_account(acc.pubkey());
    data.set_confined(true);
    env.accountsdb.insert_account(&acc.pubkey(), &data).unwrap();
    acc
}

#[tokio::test]
async fn test_gasless_undelegated_feepayer_modification_fails() {
    let env = ExecutionTestEnv::new_with_config(0, 1, false);

    // 1. Configure Payer: Owned by Guinea (to allow transfer), Undelegated
    {
        let mut payer = env.get_payer();
        payer.set_owner(guinea::ID);
        payer.set_delegated(false);
        payer.commit();
    }

    // 2. Execute Transfer (Payer -> Recipient)
    let recipient = env.create_account(100);
    let ix = transfer_ix(env.get_payer().pubkey, recipient.pubkey(), 1000);
    let txn = env.build_transaction(&[ix]);

    // 3. Assert Failure: Undelegated fee payer cannot be modified in gasless mode
    let result = env.execute_transaction(txn).await;
    assert_eq!(result.unwrap_err(), TransactionError::InvalidAccountForFee);
}

#[tokio::test]
async fn test_confined_account_lamport_modification_fails() {
    let env = ExecutionTestEnv::new();
    let confined = setup_confined(&env);
    let recipient = env.create_account(100);

    // Attempt to move funds FROM confined account
    let mut ix = transfer_ix(confined.pubkey(), recipient.pubkey(), 100);
    ix.accounts.first_mut().unwrap().is_signer = false;

    let txn = env.build_transaction(&[ix]);

    let result = env.execute_transaction(txn).await;
    assert_eq!(
        result.unwrap_err(),
        TransactionError::UnbalancedTransaction,
        "Confined account balance cannot change"
    );
}

#[tokio::test]
async fn test_confined_account_data_modification_succeeds() {
    let env = ExecutionTestEnv::new();
    let confined = setup_confined(&env);

    let ix = Instruction::new_with_bincode(
        guinea::ID,
        &GuineaInstruction::WriteByteToData(99),
        vec![AccountMeta::new(confined.pubkey(), false)],
    );

    let txn = env.build_transaction(&[ix]);
    assert!(env.execute_transaction(txn).await.is_ok());

    let acc = env.get_account(confined.pubkey());
    assert_eq!(acc.data()[0], 99, "Data modification should be allowed");
}
