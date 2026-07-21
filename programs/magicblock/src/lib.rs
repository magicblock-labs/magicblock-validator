mod ephemeral_accounts;
pub mod errors;
mod magic_context;
pub mod magic_sys;
mod schedule_task;
mod schedule_transactions;
pub use magic_context::MagicContext;
pub mod magic_scheduled_base_intent;
pub mod magicblock_processor;
pub mod test_utils;
mod utils;
pub mod validator;

pub use magic_sys::init_magic_sys;
pub use magicblock_magic_program_api::*;
pub use schedule_transactions::{
    SentCommit, process_scheduled_commit_sent, register_scheduled_commit_sent,
    transaction_scheduler::TransactionScheduler,
};
pub use utils::instruction_utils;
