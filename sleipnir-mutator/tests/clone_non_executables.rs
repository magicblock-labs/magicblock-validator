use log::*;
use test_tools::init_logger;

use sleipnir_mutator::Mutator;
use sleipnir_program::sleipnir_authority_id;
use solana_sdk::{genesis_config::ClusterType, hash::Hash};
use test_tools::account::{fund_account_addr, get_account_addr};
use test_tools::bank_transactions_processor::BankTransactionsProcessor;
use test_tools::banking_stage_transactions_processor::BankingStageTransactionsProcessor;
use test_tools::diagnostics::log_exec_details;
use test_tools::traits::TransactionsProcessor;

const SOLX_TIPS: &str = "SoLXtipsYqzgFguFCX6vw3JCtMChxmMacWdTpz2noRX";
const LUZIFER: &str = "LuzifKo4E6QCF5r4uQmqbyko7zLS5WgayynivnCbtzk";

fn transactions_processor() -> Box<dyn TransactionsProcessor> {
    if std::env::var("BANK_PROCESSOR").is_ok() {
        Box::new(BankTransactionsProcessor::default())
    } else {
        Box::new(BankingStageTransactionsProcessor::default())
    }
}

#[tokio::test]
async fn clone_non_executable_without_data() {
    init_logger!();
    let mutator = Mutator::default();

    let recent_blockhash = Hash::default();
    let tx = mutator
        .transaction_to_clone_account_from_cluster(ClusterType::Devnet, SOLX_TIPS, recent_blockhash)
        .await
        .expect("Failed to create clone transaction");

    assert!(tx.is_signed());
    assert_eq!(tx.signatures.len(), 1);
    assert_eq!(tx.signer_key(0, 0).unwrap(), &sleipnir_authority_id());
    assert_eq!(tx.message().account_keys.len(), 3);

    let tx_processor = transactions_processor();

    // TODO: we need to fund Luzifer at startup instead of doing it here
    fund_account_addr(tx_processor.bank(), LUZIFER, u64::MAX / 2);

    let result = tx_processor.process(vec![tx]).unwrap();

    let (_, exec_details) = result.transactions.values().next().unwrap();
    log_exec_details(exec_details);
    let solx_tips = get_account_addr(tx_processor.bank(), SOLX_TIPS).unwrap();
    trace!("SolxTips account: {:#?}", solx_tips);
}
