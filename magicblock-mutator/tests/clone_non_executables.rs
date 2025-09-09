use assert_matches::assert_matches;
use log::*;
use magicblock_mutator::fetch::transaction_to_clone_pubkey_from_cluster;
use magicblock_program::{
    test_utils::ensure_started_validator,
    validator::{self, validator_authority_id},
};
use solana_sdk::{
    account::Account, clock::Slot, genesis_config::ClusterType, hash::Hash,
    native_token::LAMPORTS_PER_SOL, pubkey::Pubkey, system_program,
    transaction::Transaction,
};
use test_kit::{skip_if_devnet_down, ExecutionTestEnv};
use utils::LUZIFER;

use crate::utils::{SOLX_POST, SOLX_PROG, SOLX_TIPS};

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
    skip_if_devnet_down!();
    ensure_started_validator(&mut Default::default());

    let test_env = ExecutionTestEnv::new();

    test_env.fund_account(LUZIFER, u64::MAX / 2);
    test_env.fund_account(validator_authority_id(), u64::MAX / 2);
    let mut authority = test_env.get_account(validator_authority_id());
    authority.as_borrowed_mut().unwrap().set_privileged(true);
    authority.commmit();
    let slot = test_env.advance_slot();

    let txn = verified_tx_to_clone_non_executable_from_devnet(
        &SOLX_TIPS,
        slot,
        test_env.ledger.latest_blockhash(),
    )
    .await;
    test_env
        .execute_transaction(txn)
        .await
        .expect("failed to clone non-exec account from devnet");

    let solx_tips = test_env.get_account(SOLX_TIPS).account.into();

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
    skip_if_devnet_down!();
    ensure_started_validator(&mut Default::default());

    let test_env = ExecutionTestEnv::new();

    test_env.fund_account(LUZIFER, u64::MAX / 2);
    test_env.fund_account(validator_authority_id(), u64::MAX / 2);
    let mut authority = test_env.get_account(validator_authority_id());
    authority.as_borrowed_mut().unwrap().set_privileged(true);
    authority.commmit();
    let slot = test_env.advance_slot();
    let txn = verified_tx_to_clone_non_executable_from_devnet(
        &SOLX_POST,
        slot,
        test_env.ledger.latest_blockhash(),
    )
    .await;
    test_env
        .execute_transaction(txn)
        .await
        .expect("failed to clone non-exec account with data from devnet");

    let solx_post = test_env.accountsdb.get_account(&SOLX_POST).unwrap().into();

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
