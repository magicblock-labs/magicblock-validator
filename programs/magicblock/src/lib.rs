pub mod errors;
mod magic_context;
mod mutate_accounts;
mod schedule_transactions;
pub use magic_context::{FeePayerAccount, MagicContext};
pub mod args;
pub mod magic_scheduled_base_intent;
pub mod magicblock_instruction;
// TODO(edwin): isolate with features
pub mod magicblock_processor;
#[cfg(test)]
mod test_utils;
mod utils;
pub mod validator;

pub use magicblock_core::magic_program::*;
pub use mutate_accounts::*;
pub use schedule_transactions::{
    process_scheduled_commit_sent, register_scheduled_commit_sent,
    transaction_scheduler::TransactionScheduler, SentCommit,
};
pub use utils::instruction_utils;
