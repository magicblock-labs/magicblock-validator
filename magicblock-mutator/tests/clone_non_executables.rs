use assert_matches::assert_matches;
use log::*;
use magicblock_mutator::fetch::transaction_to_clone_pubkey_from_cluster;
use magicblock_program::validator;
use solana_sdk::{
    account::Account, clock::Slot, genesis_config::ClusterType, hash::Hash,
    native_token::LAMPORTS_PER_SOL, pubkey::Pubkey, system_program,
    transaction::Transaction,
};
use test_tools::{
    diagnostics::log_exec_details, init_logger, skip_if_devnet_down,
    validator::init_started_validator,
};

use crate::utils::{fund_luzifer, SOLX_POST, SOLX_PROG, SOLX_TIPS};

mod utils;

async fn verified_tx_to_clone_non_executable_from_devnet(
    pubkey: &Pubkey,
    slot: Slot,
    recent_blockhash: Hash,
) -> Transaction {
    let tx = transaction_to_clone_pubkey_from_cluster(
        &ClusterType::Devnet.into(),
        false,
        pubkey,
        recent_blockhash,
        slot,
        None,
    )
    .await
    .expect("Failed to create clone transaction");

    assert!(tx.is_signed());
    assert_eq!(tx.signatures.len(), 1);
    assert_eq!(
        tx.signer_key(0, 0).unwrap(),
        &validator::validator_authority_id()
    );
    assert_eq!(tx.message().account_keys.len(), 3);

    tx
}

#[tokio::test]
async fn clone_non_executable_without_data() {
    init_logger!();
    skip_if_devnet_down!();

    let tx_processor = transactions_processor();
    init_started_validator(tx_processor.bank());
    fund_luzifer(&*tx_processor);

    let slot = tx_processor.bank().slot();
    let tx = verified_tx_to_clone_non_executable_from_devnet(
        &SOLX_TIPS,
        slot,
        tx_processor.bank().last_blockhash(),
    )
    .await;
    let result = tx_processor.process(vec![tx]).unwrap();

    let (_, exec_details) = result.transactions.values().next().unwrap();
    log_exec_details(exec_details);
    let solx_tips = tx_processor.bank().get_account(&SOLX_TIPS).unwrap().into();

    trace!("SolxTips account: {:#?}", solx_tips);

    assert_matches!(
        solx_tips,
        Account {
            lamports,
            data,
            owner,
            executable: false,
            rent_epoch
        } => {
            assert!(lamports > LAMPORTS_PER_SOL);
            assert!(data.is_empty());
            assert_eq!(owner, system_program::id());
            assert_eq!(rent_epoch, u64::MAX);
        }
    );
}

#[tokio::test]
async fn clone_non_executable_with_data() {
    init_logger!();
    skip_if_devnet_down!();

    let tx_processor = transactions_processor();
    init_started_validator(tx_processor.bank());
    fund_luzifer(&*tx_processor);

    let slot = tx_processor.bank().slot();
    let tx = verified_tx_to_clone_non_executable_from_devnet(
        &SOLX_POST,
        slot,
        tx_processor.bank().last_blockhash(),
    )
    .await;
    let result = tx_processor.process(vec![tx]).unwrap();

    let (_, exec_details) = result.transactions.values().next().unwrap();
    log_exec_details(exec_details);
    let solx_post = tx_processor.bank().get_account(&SOLX_POST).unwrap().into();

    trace!("SolxPost account: {:#?}", solx_post);

    assert_matches!(
        solx_post,
        Account {
            lamports,
            data,
            owner,
            executable: false,
            rent_epoch
        } => {
            assert!(lamports > 0);
            assert_eq!(data.len(), 1180);
            assert_eq!(owner, SOLX_PROG);
            assert_eq!(rent_epoch, u64::MAX);
        }
    );
}
