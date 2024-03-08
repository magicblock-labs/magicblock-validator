use bank_transactions_processor::BankTransactionsProcessor;
use banking_stage_transactions_processor::BankingStageTransactionsProcessor;
use traits::TransactionsProcessor;

pub mod account;
pub mod bank_transactions_processor;
pub mod banking_stage_transactions_processor;
pub use test_tools_core::*;
pub mod traits;
pub mod transaction;

pub fn transactions_processor() -> Box<dyn TransactionsProcessor> {
    if std::env::var("PROCESSOR_BANK").is_ok() {
        Box::<BankTransactionsProcessor>::default()
    } else {
        Box::<BankingStageTransactionsProcessor>::default()
    }
}
