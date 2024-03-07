use log::*;
use solana_sdk::account::Account;
use solana_sdk::native_token::LAMPORTS_PER_SOL;
use solana_sdk::system_program;
use solana_sdk::transaction::Transaction;
use test_tools::{init_logger, transactions_processor};

use assert_matches::assert_matches;
use sleipnir_mutator::Mutator;
use sleipnir_program::sleipnir_authority_id;
use solana_sdk::{genesis_config::ClusterType, hash::Hash};
use test_tools::account::{fund_account_addr, get_account_addr};
use test_tools::diagnostics::log_exec_details;
use test_tools::traits::TransactionsProcessor;

const SOLX_TIPS: &str = "SoLXtipsYqzgFguFCX6vw3JCtMChxmMacWdTpz2noRX";
const LUZIFER: &str = "LuzifKo4E6QCF5r4uQmqbyko7zLS5WgayynivnCbtzk";

fn fund_luzifer(bank: &Box<dyn TransactionsProcessor>) {
    // TODO: we need to fund Luzifer at startup instead of doing it here
    fund_account_addr(bank.bank(), LUZIFER, u64::MAX / 2);
}

async fn verified_tx_to_clone_from_devnet(addr: &str) -> Transaction {
    let mutator = Mutator::default();

    let recent_blockhash = Hash::default();
    let tx = mutator
        .transaction_to_clone_account_from_cluster(ClusterType::Devnet, addr, recent_blockhash)
        .await
        .expect("Failed to create clone transaction");

    assert!(tx.is_signed());
    assert_eq!(tx.signatures.len(), 1);
    assert_eq!(tx.signer_key(0, 0).unwrap(), &sleipnir_authority_id());
    assert_eq!(tx.message().account_keys.len(), 3);

    tx
}

#[tokio::test]
async fn clone_non_executable_without_data() {
    init_logger!();

    let tx_processor = transactions_processor();
    fund_luzifer(&tx_processor);

    let tx = verified_tx_to_clone_from_devnet(SOLX_TIPS).await;
    let result = tx_processor.process(vec![tx]).unwrap();

    let (_, exec_details) = result.transactions.values().next().unwrap();
    log_exec_details(exec_details);
    let solx_tips: Account = get_account_addr(tx_processor.bank(), SOLX_TIPS)
        .unwrap()
        .into();

    trace!("SolxTips account: {:#?}", solx_tips);

    assert_matches!(
        solx_tips,
        Account {
            lamports: l,
            data: d,
            owner: o,
            executable: false,
            rent_epoch: r
        } => {
            assert!(l > LAMPORTS_PER_SOL);
            assert!(d.is_empty());
            assert_eq!(o, system_program::id());
            assert_eq!(r, u64::MAX);
        }
    );
}
