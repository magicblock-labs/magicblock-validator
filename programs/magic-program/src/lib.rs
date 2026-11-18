#![doc = include_str!("../README.md")]

pub mod errors;
mod magic_accounts;
mod magic_context;
mod schedule_task;
pub use magic_context::MagicContext;
mod intent_bundles;
pub mod magicblock_processor;
mod outbox_intent;
pub mod test_utils;
mod utils;
pub mod validator;
pub use intent_bundles::{magic_scheduled_base_intent, schedule as schedule_transactions};
pub use magic_program_api::outbox::SentCommit;
pub use magic_program_api::*;
pub use outbox_intent::outbox_intent_bundles;
pub use schedule_transactions::transaction_scheduler::TransactionScheduler;
pub use utils::instruction_utils;
