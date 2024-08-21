use sleipnir_mutator::mutator;
use sleipnir_program::validator_authority_id;
use solana_program::pubkey;
use solana_sdk::{
    clock::Slot, genesis_config::ClusterType, hash::Hash, pubkey::Pubkey,
    transaction::Transaction,
};
use test_tools::{account::fund_account_addr, traits::TransactionsProcessor};

pub const SOLX_PROG: Pubkey =
    pubkey!("SoLXmnP9JvL6vJ7TN1VqtTxqsc2izmPfF9CsMDEuRzJ");
#[allow(dead_code)] // used in tests
pub const SOLX_EXEC: Pubkey =
    pubkey!("J1ct2BY6srXCDMngz5JxkX3sHLwCqGPhy9FiJBc8nuwk");
#[allow(dead_code)] // used in tests
pub const SOLX_IDL: Pubkey =
    pubkey!("EgrsyMAsGYMKjcnTvnzmpJtq3hpmXznKQXk21154TsaS");
#[allow(dead_code)] // used in tests
pub const SOLX_TIPS: Pubkey =
    pubkey!("SoLXtipsYqzgFguFCX6vw3JCtMChxmMacWdTpz2noRX");
#[allow(dead_code)] // used in tests
pub const SOLX_POST: Pubkey =
    pubkey!("5eYk1TwtEwsUTqF9FHhm6tdmvu45csFkKbC4W217TAts");
const LUZIFER: Pubkey = pubkey!("LuzifKo4E6QCF5r4uQmqbyko7zLS5WgayynivnCbtzk");

pub fn fund_luzifer(bank: &dyn TransactionsProcessor) {
    // TODO: we need to fund Luzifer at startup instead of doing it here
    fund_account_addr(bank.bank(), &LUZIFER, u64::MAX / 2);
}

pub async fn verified_tx_to_clone_from_devnet(
    pubkey: &Pubkey,
    slot: Slot,
    num_accounts_expected: usize,
    recent_blockhash: Hash,
) -> Transaction {
    let txs = mutator::transactions_to_clone_account_from_cluster(
        &ClusterType::Devnet.into(),
        pubkey,
        None,
        recent_blockhash,
        slot,
        None,
    )
    .await
    .expect("Failed to create clone transaction");

    let first = txs.first().unwrap();

    assert!(first.is_signed());
    assert_eq!(first.signatures.len(), 1);
    assert_eq!(first.signer_key(0, 0).unwrap(), &validator_authority_id());
    assert_eq!(first.message().account_keys.len(), num_accounts_expected);

    // TODO(vbrunet) - update the tests to care about the whole list of TX, not just the first one
    first.clone()
}
