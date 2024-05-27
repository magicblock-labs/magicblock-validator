use sleipnir_mutator::mutator;
use sleipnir_program::validator_authority_id;
use solana_sdk::{
    clock::Slot, genesis_config::ClusterType, hash::Hash,
    transaction::Transaction,
};
use test_tools::{account::fund_account_addr, traits::TransactionsProcessor};

pub const SOLX_PROG: &str = "SoLXmnP9JvL6vJ7TN1VqtTxqsc2izmPfF9CsMDEuRzJ";
#[allow(dead_code)] // used in tests
pub const SOLX_EXEC: &str = "J1ct2BY6srXCDMngz5JxkX3sHLwCqGPhy9FiJBc8nuwk";
#[allow(dead_code)] // used in tests
pub const SOLX_IDL: &str = "EgrsyMAsGYMKjcnTvnzmpJtq3hpmXznKQXk21154TsaS";
#[allow(dead_code)] // used in tests
pub const SOLX_TIPS: &str = "SoLXtipsYqzgFguFCX6vw3JCtMChxmMacWdTpz2noRX";
#[allow(dead_code)] // used in tests
pub const SOLX_POST: &str = "5eYk1TwtEwsUTqF9FHhm6tdmvu45csFkKbC4W217TAts";
const LUZIFER: &str = "LuzifKo4E6QCF5r4uQmqbyko7zLS5WgayynivnCbtzk";

pub fn fund_luzifer(bank: &dyn TransactionsProcessor) {
    // TODO: we need to fund Luzifer at startup instead of doing it here
    fund_account_addr(bank.bank(), LUZIFER, u64::MAX / 2);
}

pub async fn verified_tx_to_clone_from_devnet(
    addr: &str,
    slot: Slot,
    num_accounts_expected: usize,
) -> Transaction {
    let recent_blockhash = Hash::default();
    let tx = mutator::transaction_to_clone_account_from_cluster(
        &ClusterType::Devnet.into(),
        addr,
        recent_blockhash,
        slot,
        None,
    )
    .await
    .expect("Failed to create clone transaction");

    assert!(tx.is_signed());
    assert_eq!(tx.signatures.len(), 1);
    assert_eq!(tx.signer_key(0, 0).unwrap(), &validator_authority_id());
    assert_eq!(tx.message().account_keys.len(), num_accounts_expected);

    tx
}
