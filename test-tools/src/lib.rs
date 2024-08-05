use bank_transactions_processor::BankTransactionsProcessor;
use traits::TransactionsProcessor;

pub mod account;
pub mod bank;
pub mod bank_transactions_processor;
pub use test_tools_core::*;
pub mod programs;
pub mod services;
pub mod traits;
pub mod transaction;
pub mod validator;

pub fn transactions_processor() -> Box<dyn TransactionsProcessor> {
    Box::<BankTransactionsProcessor>::default()
}
