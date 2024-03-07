use bank_transactions_processor::BankTransactionsProcessor;
use banking_stage_transactions_processor::BankingStageTransactionsProcessor;
use traits::TransactionsProcessor;

pub mod account;
pub mod bank_transactions_processor;
pub mod banking_stage_transactions_processor;
pub mod diagnostics;
pub mod traits;

pub fn transactions_processor() -> Box<dyn TransactionsProcessor> {
    if std::env::var("PROCESSOR_BANK").is_ok() {
        Box::new(BankTransactionsProcessor::default())
    } else {
        Box::new(BankingStageTransactionsProcessor::default())
    }
}
