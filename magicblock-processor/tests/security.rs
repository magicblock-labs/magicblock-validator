use guinea::GuineaInstruction;
use solana_account::{ReadableAccount, WritableAccount};
use solana_program::{
    instruction::{AccountMeta, Instruction},
    native_token::LAMPORTS_PER_SOL,
};
use solana_signer::Signer;
use solana_transaction_error::TransactionError;
use test_kit::ExecutionTestEnv;

/// Verifies that in gasless mode (0 fees), a transaction fails
/// if it modifies an undelegated fee payer.
#[tokio::test]
async fn test_gasless_undelegated_feepayer_modification_fails() {
    let env = ExecutionTestEnv::new_with_config(0);

    {
        let mut account = env.get_payer();
        account.set_owner(guinea::ID);
        account.set_delegated(false);
        account.commmit();
    }
    env.advance_slot();

    // 3. Create a recipient (also owned by Guinea for the Transfer instruction compatibility)
    let recipient = env.create_account_with_config(100, 42, guinea::ID);

    // 4. Create Transfer instruction: Payer -> Recipient
    // This modifies the payer's lamports.
    let ix = Instruction::new_with_bincode(
        guinea::ID,
        &GuineaInstruction::Transfer(1000),
        vec![
            AccountMeta::new(env.payer.pubkey(), true),
            AccountMeta::new(recipient.pubkey(), false),
        ],
    );

    // 5. Build transaction using the default payer
    let txn = env.build_transaction(&[ix]);

    // 6. Execute and Assert Failure
    let result = env.execute_transaction(txn).await;
    assert!(
        result.is_err(),
        "Modification of undelegated fee payer in gasless mode should fail"
    );
    assert_eq!(
        result.unwrap_err(),
        TransactionError::InvalidAccountForFee,
        "Expected InvalidAccountForFee error"
    );
}

/// Verifies that modifying the lamports of a confined
/// account causes the transaction to fail.
#[tokio::test]
async fn test_confined_account_lamport_modification_fails() {
    let env = ExecutionTestEnv::new(); // Standard fees

    let confined_sender = env.create_account_with_config(100, 42, guinea::ID);
    {
        let mut account = env.get_account(confined_sender.pubkey());
        account.set_confined(true);
        account.commmit();
    }
    env.advance_slot();

    let recipient = env.create_account_with_config(100, 42, guinea::ID);

    let ix = Instruction::new_with_bincode(
        guinea::ID,
        &GuineaInstruction::Transfer(100),
        vec![
            AccountMeta::new(confined_sender.pubkey(), false), // Writable, Not Signer
            AccountMeta::new(recipient.pubkey(), false),       // Writable
        ],
    );

    let txn = env.build_transaction(&[ix]);

    let result = env.execute_transaction(txn).await;

    assert!(
        result.is_err(),
        "Modifying lamports of a confined account should fail"
    );
    assert_eq!(result.unwrap_err(), TransactionError::UnbalancedTransaction);
}

/// Verifies that modifying ONLY the DATA of a confined account SUCCEEDS.
#[tokio::test]
async fn test_confined_account_data_modification_succeeds() {
    let env = ExecutionTestEnv::new();

    let confined_account =
        env.create_account_with_config(LAMPORTS_PER_SOL, 42, guinea::ID);
    {
        let mut account = env.get_account(confined_account.pubkey());
        account.set_confined(true);
        account.commmit();
    }
    env.advance_slot();

    let ix = Instruction::new_with_bincode(
        guinea::ID,
        &GuineaInstruction::WriteByteToData(42),
        vec![AccountMeta::new(confined_account.pubkey(), false)],
    );

    let txn = env.build_transaction(&[ix]);

    let result = env.execute_transaction(txn).await;

    assert!(
        result.is_ok(),
        "Data-only modification of confined account should succeed"
    );

    let acc = env.get_account(confined_account.pubkey());
    assert_eq!(acc.data()[0], 42, "Data should be updated");
}
