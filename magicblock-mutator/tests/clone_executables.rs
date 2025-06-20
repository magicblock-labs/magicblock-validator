use assert_matches::assert_matches;
use log::*;
use magicblock_bank::{
    bank_dev_utils::{
        elfs,
        transactions::{
            create_solx_send_post_transaction, SolanaxPostAccounts,
        },
    },
    DEFAULT_LAMPORTS_PER_SIGNATURE,
};
use magicblock_mutator::fetch::transaction_to_clone_pubkey_from_cluster;
use magicblock_program::validator;
use solana_sdk::{
    account::{Account, ReadableAccount},
    bpf_loader_upgradeable,
    clock::Slot,
    genesis_config::ClusterType,
    hash::Hash,
    native_token::LAMPORTS_PER_SOL,
    pubkey::Pubkey,
    system_program,
    transaction::Transaction,
};
use test_tools::{
    diagnostics::log_exec_details, init_logger, services::skip_if_devnet_down,
    transactions_processor, validator::init_started_validator,
};

use crate::utils::{fund_luzifer, SOLX_EXEC, SOLX_IDL, SOLX_PROG};

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
    init_logger!();
    skip_if_devnet_down!();

    let tx_processor = transactions_processor();
    init_started_validator(tx_processor.bank());
    fund_luzifer(&*tx_processor);

    tx_processor.bank().advance_slot(); // We don't want to stay on slot 0

    // 1. Exec Clone Transaction
    {
        let slot = tx_processor.bank().slot();
        let tx = verified_tx_to_clone_executable_from_devnet_first_deploy(
            &SOLX_PROG,
            slot,
            tx_processor.bank().last_blockhash(),
        )
        .await;
        let result = tx_processor.process(vec![tx]).unwrap();

        let (_, exec_details) = result.transactions.values().next().unwrap();
        log_exec_details(exec_details);
    }

    // 2. Verify that all accounts were added to the validator
    {
        let solx_prog =
            tx_processor.bank().get_account(&SOLX_PROG).unwrap().into();
        trace!("SolxProg account: {:#?}", solx_prog);

        let solx_exec =
            tx_processor.bank().get_account(&SOLX_EXEC).unwrap().into();
        trace!("SolxExec account: {:#?}", solx_exec);

        let solx_idl =
            tx_processor.bank().get_account(&SOLX_IDL).unwrap().into();
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
                assert_eq!(owner, elfs::solanax::id());
                assert_eq!(rent_epoch, u64::MAX);
            }
        );
    }

    // 3. Run a transaction against the cloned program
    {
        let (tx, SolanaxPostAccounts { author, post }) =
            create_solx_send_post_transaction(tx_processor.bank());
        let sig = *tx.signature();

        let result = tx_processor.process_sanitized(vec![tx]).unwrap();
        assert_eq!(result.len(), 1);

        // Transaction
        let (tx, exec_details) = result.transactions.get(&sig).unwrap();

        log_exec_details(exec_details);
        assert!(exec_details.status.is_ok());
        assert_eq!(tx.signatures().len(), 2);
        assert_eq!(tx.message().account_keys().len(), 4);

        // Signature Status
        let sig_status = tx_processor.bank().get_signature_status(&sig);
        assert!(sig_status.is_some());
        assert_matches!(sig_status.as_ref().unwrap(), Ok(()));

        // Accounts checks
        let author_acc = tx_processor.bank().get_account(&author).unwrap();
        assert_eq!(author_acc.data().len(), 0);
        assert_eq!(author_acc.owner(), &system_program::ID);
        assert_eq!(
            author_acc.lamports(),
            LAMPORTS_PER_SOL - 2 * DEFAULT_LAMPORTS_PER_SIGNATURE
        );

        let post_acc = tx_processor.bank().get_account(&post).unwrap();
        assert_eq!(post_acc.data().len(), 1180);
        assert_eq!(post_acc.owner(), &elfs::solanax::ID);
        assert_eq!(post_acc.lamports(), 9103680);
    }

    // 4. Exec Upgrade Transactions
    {
        let slot = tx_processor.bank().slot();
        let tx = verified_tx_to_clone_executable_from_devnet_as_upgrade(
            &SOLX_PROG,
            slot,
            tx_processor.bank().last_blockhash(),
        )
        .await;
        let result = tx_processor.process(vec![tx]).unwrap();

        let (_, exec_details) = result.transactions.values().next().unwrap();
        log_exec_details(exec_details);
    }

    // 5. Run a transaction against the upgraded program
    {
        // For an upgraded program: `effective_slot = deployed_slot + 1`
        // Therefore to activate it we need to advance a slot
        tx_processor.bank().advance_slot();

        let (tx, SolanaxPostAccounts { author, post }) =
            create_solx_send_post_transaction(tx_processor.bank());
        let sig = *tx.signature();

        let result = tx_processor.process_sanitized(vec![tx]).unwrap();
        assert_eq!(result.len(), 1);

        // Transaction
        let (tx, exec_details) = result.transactions.get(&sig).unwrap();

        log_exec_details(exec_details);
        assert!(exec_details.status.is_ok());
        assert_eq!(tx.signatures().len(), 2);
        assert_eq!(tx.message().account_keys().len(), 4);

        // Signature Status
        let sig_status = tx_processor.bank().get_signature_status(&sig);
        assert!(sig_status.is_some());
        assert_matches!(sig_status.as_ref().unwrap(), Ok(()));

        // Accounts checks
        let author_acc = tx_processor.bank().get_account(&author).unwrap();
        assert_eq!(author_acc.data().len(), 0);
        assert_eq!(author_acc.owner(), &system_program::ID);
        assert_eq!(
            author_acc.lamports(),
            LAMPORTS_PER_SOL - 2 * DEFAULT_LAMPORTS_PER_SIGNATURE
        );

        let post_acc = tx_processor.bank().get_account(&post).unwrap();
        assert_eq!(post_acc.data().len(), 1180);
        assert_eq!(post_acc.owner(), &elfs::solanax::ID);
        assert_eq!(post_acc.lamports(), 9103680);
    }
}
