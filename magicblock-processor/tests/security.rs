use guinea::GuineaInstruction;
use solana_account::{ReadableAccount, WritableAccount};
use solana_keypair::Keypair;
use solana_message::{v1, VersionedMessage};
use solana_program::{
    instruction::{AccountMeta, Instruction},
    native_token::LAMPORTS_PER_SOL,
    rent::Rent,
};
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;
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
    let env = ExecutionTestEnv::new_with_config(1, false);

    // 1. Configure Payer: Owned by Guinea (to allow transfer), Undelegated
    {
        let mut payer = env.get_payer();
        payer.set_owner(guinea::ID);
        payer.set_delegated(false);
        payer.commit();
    }

    // 2. Execute Transfer (Payer -> Recipient)
    let payer_balance = env.get_payer().lamports();
    let recipient = env.create_account(LAMPORTS_PER_SOL);
    let ix = transfer_ix(env.get_payer().pubkey, recipient.pubkey(), 1000);
    let message = v1::Message::try_compile_with_config(
        &env.payers[0].pubkey(),
        &[ix],
        env.ledger.latest_blockhash(),
        v1::TransactionConfig::default()
            .with_compute_unit_limit(100_000)
            .with_loaded_accounts_data_size_limit(1_000_000),
    )
    .unwrap();
    let txn = VersionedTransaction::try_new(
        VersionedMessage::V1(message),
        &[&env.payers[0]],
    )
    .unwrap();

    // 3. Assert Failure: Undelegated fee payer cannot be modified in gasless mode
    let result = env.execute_transaction(txn).await;
    assert_eq!(result.unwrap_err(), TransactionError::InvalidAccountForFee);
    assert_eq!(env.get_payer().lamports(), payer_balance);
    assert_eq!(
        env.get_account(recipient.pubkey()).lamports(),
        LAMPORTS_PER_SOL
    );
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

/// Confined data remains writable, but growth and shrinkage fail atomically.
#[tokio::test]
async fn test_confined_account_data_modification_succeeds() {
    let env = ExecutionTestEnv::new();
    let confined = setup_confined(&env);
    let balance = Rent::default().minimum_balance(42);
    {
        // Guinea's resize instruction adjusts both accounts' rent balances.
        let mut payer = env.get_payer();
        payer.set_owner(guinea::ID);
        payer.commit();
    }
    {
        let mut account = env.get_account(confined.pubkey());
        account.set_lamports(balance);
        account.commit();
    }

    let ix = Instruction::new_with_bincode(
        guinea::ID,
        &GuineaInstruction::WriteByteToData(99),
        vec![AccountMeta::new(confined.pubkey(), false)],
    );

    let txn = env.build_transaction(&[ix]);
    assert!(env.execute_transaction(txn).await.is_ok());

    let acc = env.get_account(confined.pubkey());
    assert_eq!(acc.data()[0], 99, "Data modification should be allowed");
    let data = acc.data().to_vec();
    drop(acc);

    for size in [41, 43, 42] {
        let ix = Instruction::new_with_bincode(
            guinea::ID,
            &GuineaInstruction::Resize(size),
            vec![
                AccountMeta::new(env.payers[0].pubkey(), true),
                AccountMeta::new(confined.pubkey(), false),
            ],
        );
        let payer_balance = env.get_payer().lamports();
        let result =
            env.execute_transaction(env.build_transaction(&[ix])).await;
        if size == data.len() {
            assert!(result.is_ok());
        } else {
            // Guinea adjusts rent before resizing, so the confined balance
            // check takes precedence over the runtime's resize error.
            assert_eq!(
                result.unwrap_err(),
                TransactionError::UnbalancedTransaction
            );
        }
        let account = env.get_account(confined.pubkey());
        assert_eq!(account.data(), data);
        assert_eq!(account.lamports(), balance);
        assert!(account.confined());
        assert_eq!(env.get_payer().lamports(), payer_balance);
    }
}
