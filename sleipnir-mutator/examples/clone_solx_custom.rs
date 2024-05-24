use sleipnir_bank::bank_dev_utils::transactions::{
    create_solx_send_post_transaction, SolanaxPostAccounts,
};
use sleipnir_mutator::{mutator, Cluster};
use test_tools::{
    account::fund_account_addr, diagnostics::log_exec_details, init_logger,
    traits::TransactionsProcessor, transactions_processor,
};

pub const SOLX_PROG: &str = "SoLXmnP9JvL6vJ7TN1VqtTxqsc2izmPfF9CsMDEuRzJ";
const LUZIFER: &str = "LuzifKo4E6QCF5r4uQmqbyko7zLS5WgayynivnCbtzk";

fn fund_luzifer(bank: &dyn TransactionsProcessor) {
    // TODO: we need to fund Luzifer at startup instead of doing it here
    fund_account_addr(bank.bank(), LUZIFER, u64::MAX / 2);
}

// IMPORTANT: Make sure to start a local validator/preferably Luzid and clone the
// SolX program into it before running this example

#[tokio::main]
async fn main() {
    init_logger!();

    let tx_processor = transactions_processor();
    fund_luzifer(&*tx_processor);

    // 1. Exec Clone Transaction
    {
        let tx = {
            let slot = tx_processor.bank().slot();
            let recent_blockhash = tx_processor.bank().last_blockhash();
            mutator::transaction_to_clone_account_from_cluster(
                // We could also use Cluster::Development here which has the same URL
                // but wanted to demonstrate using a custom URL
                &Cluster::Custom("http://localhost:8899".to_string()),
                SOLX_PROG,
                recent_blockhash,
                slot,
                None,
            )
            .await
            .expect("Failed to create clone transaction")
        };

        let result = tx_processor.process(vec![tx]).unwrap();

        let (_, exec_details) = result.transactions.values().next().unwrap();
        log_exec_details(exec_details);
    }

    // For a deployed program: `effective_slot = deployed_slot + 1`
    // Therefore to activate it we need to advance a slot
    tx_processor.bank().advance_slot();

    // 2. Run a transaction against it
    let (tx, SolanaxPostAccounts { author: _, post: _ }) =
        create_solx_send_post_transaction(tx_processor.bank());
    let sig = *tx.signature();

    let result = tx_processor.process_sanitized(vec![tx]).unwrap();
    let (_, exec_details) = result.transactions.get(&sig).unwrap();

    log_exec_details(exec_details);
}
