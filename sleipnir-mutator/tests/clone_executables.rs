use assert_matches::assert_matches;
use log::*;
use sleipnir_bank::{
    bank_dev_utils::{
        elfs,
        transactions::{
            create_solx_send_post_transaction, SolanaxPostAccounts,
        },
    },
    transaction_results::TransactionBalancesSet,
    LAMPORTS_PER_SIGNATURE,
};
use sleipnir_mutator::transactions::transactions_to_clone_account_from_cluster;
use sleipnir_program::validator_authority_id;
use solana_sdk::{
    account::{Account, ReadableAccount},
    bpf_loader_upgradeable,
    clock::Slot,
    genesis_config::ClusterType,
    hash::Hash,
    native_token::LAMPORTS_PER_SOL,
    pubkey::Pubkey,
    transaction::Transaction,
};
use test_tools::{
    diagnostics::log_exec_details, init_logger, services::skip_if_devnet_down,
    transactions_processor, validator::ensure_funded_validator_authority,
};

use crate::utils::{fund_luzifer, SOLX_EXEC, SOLX_IDL, SOLX_PROG};

mod utils;

async fn verified_txs_to_clone_executable_from_devnet(
    pubkey: &Pubkey,
    slot: Slot,
    recent_blockhash: Hash,
) -> Vec<Transaction> {
    let txs = transactions_to_clone_account_from_cluster(
        &ClusterType::Devnet.into(),
        pubkey,
        None,
        recent_blockhash,
        slot,
        None,
    )
    .await
    .expect("Failed to create program clone transaction");

    assert!(txs.len() == 2);

    let tx_mutate = txs.first().unwrap();
    assert!(tx_mutate.is_signed());
    assert_eq!(tx_mutate.signatures.len(), 1);
    assert_eq!(
        tx_mutate.signer_key(0, 0).unwrap(),
        &validator_authority_id()
    );
    assert!(tx_mutate.message().account_keys.len() >= 5);
    assert!(tx_mutate.message().account_keys.len() <= 6);

    let tx_upgrade = txs.get(1).unwrap();
    assert!(tx_upgrade.is_signed());
    assert_eq!(tx_upgrade.signatures.len(), 1);
    /*
    assert_eq!(
        tx_upgrade.signer_key(0, 0).unwrap(),
        &validator_authority_id()
    );
     */

    txs
}

#[tokio::test]
async fn clone_solx_executable() {
    init_logger!();
    skip_if_devnet_down!();

    let tx_processor = transactions_processor();
    ensure_funded_validator_authority(tx_processor.bank());
    fund_luzifer(&*tx_processor);

    // 1. Exec Clone Transaction
    {
        let slot = tx_processor.bank().slot();
        let txs = verified_txs_to_clone_executable_from_devnet(
            &SOLX_PROG,
            slot,
            tx_processor.bank().last_blockhash(),
        )
        .await;
        let result = tx_processor.process(txs).unwrap();

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
                assert!(lamports >= 1141440);
                assert!(data.len() >= 36);
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
                assert!(lamports >= 2890996080);
                assert!(data.len() >= 415245);
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
                assert!(lamports >= 6264000);
                assert!(data.len() >= 772);
                assert_eq!(owner, elfs::solanax::id());
                assert_eq!(rent_epoch, u64::MAX);
            }
        );
    }

    // 3. Run a transaction against the cloned program
    {
        // For a deployed program: `effective_slot = deployed_slot + 1`
        // Therefore to activate it we need to advance a slot
        tx_processor.bank().advance_slot();

        let (tx, SolanaxPostAccounts { author: _, post }) =
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

        // Accounts
        let post_acc = tx_processor.bank().get_account(&post).unwrap();
        assert_eq!(post_acc.data().len(), 1180);
        assert_eq!(post_acc.owner(), &elfs::solanax::ID);

        // Balances
        assert_eq!(result.balances.len(), 1);
        let balances = &result.balances[0];
        assert_matches!(
            balances,
            TransactionBalancesSet {
                pre_balances: pre,
                post_balances: post,
            } => {
                assert_eq!(pre.len(), 1);
                assert_eq!(pre[0], [LAMPORTS_PER_SOL, 9103680, 1, 1141440]);

                assert_eq!(post.len(), 1);
                assert_eq!(post[0], [LAMPORTS_PER_SOL - 2 * LAMPORTS_PER_SIGNATURE , 9103680, 1, 1141440]);
            }
        );
    }
}
