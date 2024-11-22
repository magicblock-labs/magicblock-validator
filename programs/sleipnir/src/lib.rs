pub mod errors;
mod magic_context;
mod mutate_accounts;
mod schedule_transactions;
pub use magic_context::{MagicContext, ScheduledCommit};
pub mod sleipnir_instruction;
pub mod sleipnir_processor;
#[cfg(test)]
mod test_utils;
mod utils;
mod validator;

pub use schedule_transactions::{
    process_scheduled_commit_sent, register_scheduled_commit_sent,
    transaction_scheduler::TransactionScheduler, SentCommit,
};
pub use sleipnir_core::magic_program::*;
pub use validator::*;
