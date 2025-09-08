use assert_matches::assert_matches;
use log::*;
use magicblock_core::traits::AccountsBank;
use magicblock_mutator::fetch::transaction_to_clone_pubkey_from_cluster;
use magicblock_program::{
    test_utils::ensure_started_validator,
    validator::{self, validator_authority_id},
};
use solana_sdk::{
    account::{Account, ReadableAccount},
    bpf_loader_upgradeable,
    clock::Slot,
    genesis_config::ClusterType,
    hash::Hash,
    instruction::{AccountMeta, Instruction},
    message::Message,
    native_token::LAMPORTS_PER_SOL,
    pubkey::Pubkey,
    rent::Rent,
    signature::Keypair,
    signer::Signer,
    system_program,
    transaction::{SanitizedTransaction, Transaction},
};
use test_kit::{skip_if_devnet_down, ExecutionTestEnv};
use utils::LUZIFER;

use crate::utils::{SOLX_EXEC, SOLX_IDL, SOLX_PROG};

mod utils;

async fn verified_tx_to_clone_executable_from_devnet_first_deploy(
    pubkey: &Pubkey,
    slot: Slot,
    recent_blockhash: Hash,
) -> Transaction {
    let tx = transaction_to_clone_pubkey_from_cluster(
        &ClusterType::Devnet.into(),
        false, // We are deploying the program for the first time
        pubkey,
        recent_blockhash,
        slot,
        None,
    )
    .await
    .expect("Failed to create program clone transaction");

    assert!(tx.is_signed());
    assert_eq!(tx.signatures.len(), 1);
    assert_eq!(
        tx.signer_key(0, 0).unwrap(),
        &validator::validator_authority_id()
    );
    assert!(tx.message().account_keys.len() >= 5);
    assert!(tx.message().account_keys.len() <= 6);

    tx
}

async fn verified_tx_to_clone_executable_from_devnet_as_upgrade(
    pubkey: &Pubkey,
    slot: Slot,
    recent_blockhash: Hash,
) -> Transaction {
    let tx = transaction_to_clone_pubkey_from_cluster(
        &ClusterType::Devnet.into(),
        true, // We are upgrading the program
        pubkey,
        recent_blockhash,
        slot,
        None,
    )
    .await
    .expect("Failed to create program clone transaction");

    assert!(tx.is_signed());
    assert_eq!(tx.signatures.len(), 1);
    assert_eq!(
        tx.signer_key(0, 0).unwrap(),
        &validator::validator_authority_id()
    );
    assert!(tx.message().account_keys.len() >= 8);
    assert!(tx.message().account_keys.len() <= 9);

    tx
}

#[tokio::test]
async fn clone_executable_with_idl_and_program_data_and_then_upgrade() {
    skip_if_devnet_down!();
    ensure_started_validator(&mut Default::default());
    let test_env = ExecutionTestEnv::new();
    test_env.fund_account(LUZIFER, u64::MAX / 2);
    test_env.fund_account(validator_authority_id(), u64::MAX / 2);

    test_env.advance_slot(); // We don't want to stay on slot 0

    // 1. Exec Clone Transaction
    {
        let slot = test_env.accountsdb.slot();
        let txn = verified_tx_to_clone_executable_from_devnet_first_deploy(
            &SOLX_PROG,
            slot,
            test_env.ledger.latest_blockhash(),
        )
        .await;
        test_env
            .execute_transaction(txn)
            .await
            .expect("failed to execute clone transaction for SOLX");
    }

    // 2. Verify that all accounts were added to the validator
    {
        let solx_prog =
            test_env.accountsdb.get_account(&SOLX_PROG).unwrap().into();
        trace!("SolxProg account: {:#?}", solx_prog);

        let solx_exec =
            test_env.accountsdb.get_account(&SOLX_EXEC).unwrap().into();
        trace!("SolxExec account: {:#?}", solx_exec);

        let solx_idl =
            test_env.accountsdb.get_account(&SOLX_IDL).unwrap().into();
        trace!("SolxIdl account: {:#?}", solx_idl);

        assert_matches!(
            solx_prog,
            Account {
                lamports,
                data,
                owner,
                executable: true,
                rent_epoch
            } => {
                assert_eq!(lamports, 1141440);
                assert_eq!(data.len(), 36);
                assert_eq!(owner, bpf_loader_upgradeable::id());
                assert_eq!(rent_epoch, u64::MAX);
            }
        );
        assert_matches!(
            solx_exec,
            Account {
                lamports,
                data,
                owner,
                executable: false,
                rent_epoch
            } => {
                assert_eq!(lamports, 2890996080);
                assert_eq!(data.len(), 415245);
                assert_eq!(owner, bpf_loader_upgradeable::id());
                assert_eq!(rent_epoch, u64::MAX);
            }
        );
        assert_matches!(
            solx_idl,
            Account {
                lamports,
                data,
                owner,
                executable: false,
                rent_epoch
            } => {
                assert_eq!(lamports, 6264000);
                assert_eq!(data.len(), 772);
                assert_eq!(owner, SOLX_PROG);
                assert_eq!(rent_epoch, u64::MAX);
            }
        );
    }
    test_env.advance_slot();

    // 3. Run a transaction against the cloned program
    {
        let (txn, SolanaxPostAccounts { author, post }) =
            create_solx_send_post_transaction(&test_env);
        let sig = *txn.signature();

        assert_eq!(txn.signatures().len(), 2);
        assert_eq!(txn.message().account_keys().len(), 4);

        test_env
            .execute_transaction(txn)
            .await
            .expect("failed to execute SOLX send post transaction");

        // Signature Status
        let sig_status = test_env.get_transaction(sig);
        assert!(sig_status.is_some());

        // Accounts checks
        let author_acc = test_env.accountsdb.get_account(&author).unwrap();
        assert_eq!(author_acc.data().len(), 0);
        assert_eq!(author_acc.owner(), &system_program::ID);
        assert_eq!(author_acc.lamports(), LAMPORTS_PER_SOL);

        let post_acc = test_env.accountsdb.get_account(&post).unwrap();
        assert_eq!(post_acc.data().len(), 1180);
        assert_eq!(post_acc.owner(), &SOLX_PROG);
        assert_eq!(post_acc.lamports(), 9103680);
    }
    test_env.advance_slot();

    // 4. Exec Upgrade Transactions
    {
        let slot = test_env.accountsdb.slot();
        let txn = verified_tx_to_clone_executable_from_devnet_as_upgrade(
            &SOLX_PROG,
            slot,
            test_env.ledger.latest_blockhash(),
        )
        .await;
        test_env
            .execute_transaction(txn)
            .await
            .expect("failed to execute solx upgrade transaction");
    }

    // 5. Run a transaction against the upgraded program
    {
        // For an upgraded program: `effective_slot = deployed_slot + 1`
        // Therefore to activate it we need to advance a slot
        test_env.advance_slot();

        let (txn, SolanaxPostAccounts { author, post }) =
            create_solx_send_post_transaction(&test_env);
        let sig = *txn.signature();
        assert_eq!(txn.signatures().len(), 2);
        assert_eq!(txn.message().account_keys().len(), 4);

        test_env.execute_transaction(txn).await.expect("failed to re-run SOLX send and post transaction against an upgraded program");

        // Signature Status
        let sig_status = test_env.get_transaction(sig);
        assert!(sig_status.is_some());

        // Accounts checks
        let author_acc = test_env.accountsdb.get_account(&author).unwrap();
        assert_eq!(author_acc.data().len(), 0);
        assert_eq!(author_acc.owner(), &system_program::ID);
        assert_eq!(author_acc.lamports(), LAMPORTS_PER_SOL);

        let post_acc = test_env.accountsdb.get_account(&post).unwrap();
        assert_eq!(post_acc.data().len(), 1180);
        assert_eq!(post_acc.owner(), &SOLX_PROG);
        assert_eq!(post_acc.lamports(), 9103680);
    }
}

// SolanaX
pub struct SolanaxPostAccounts {
    pub post: Pubkey,
    pub author: Pubkey,
}
pub fn create_solx_send_post_transaction(
    test_env: &ExecutionTestEnv,
) -> (SanitizedTransaction, SolanaxPostAccounts) {
    let accounts = vec![
        test_env.create_account(Rent::default().minimum_balance(1180)),
        test_env.create_account(LAMPORTS_PER_SOL),
    ];
    let post = &accounts[0];
    let author = &accounts[1];
    let instruction = create_solx_send_post_instruction(&SOLX_PROG, &accounts);
    let message = Message::new(&[instruction], Some(&author.pubkey()));
    let transaction = Transaction::new(
        &[author, post],
        message,
        test_env.ledger.latest_blockhash(),
    );
    (
        SanitizedTransaction::try_from_legacy_transaction(
            transaction,
            &Default::default(),
        )
        .unwrap(),
        SolanaxPostAccounts {
            post: post.pubkey(),
            author: author.pubkey(),
        },
    )
}

fn create_solx_send_post_instruction(
    program_id: &Pubkey,
    funded_accounts: &[Keypair],
) -> Instruction {
    // https://explorer.solana.com/tx/nM2WLNPVfU3R8C4dJwhzwBsVXXgBkySAuBrGTEoaGaAQMxNHy4mnAgLER8ddDmD6tjw3suVhfG1RdbdbhyScwLK?cluster=devnet
    #[rustfmt::skip]
    let ix_bytes: Vec<u8> = vec![
        0x84, 0xf5, 0xee, 0x1d,
        0xf3, 0x2a, 0xad, 0x36,
        0x05, 0x00, 0x00, 0x00,
        0x68, 0x65, 0x6c, 0x6c,
        0x6f,
    ];
    Instruction::new_with_bytes(
        *program_id,
        &ix_bytes,
        vec![
            AccountMeta::new(funded_accounts[0].pubkey(), true),
            AccountMeta::new(funded_accounts[1].pubkey(), true),
            AccountMeta::new_readonly(system_program::id(), false),
        ],
    )
}
