mod clone_account;
mod ephemeral_accounts;
pub mod errors;
mod magic_context;
pub mod magic_sys;
mod mutate_accounts;
mod schedule_task;
pub use magic_context::MagicContext;
mod intent_bundles;
pub mod magicblock_processor;
mod outbox_intent;
pub mod test_utils;
mod utils;
pub mod validator;
pub use clone_account::is_pending_clone;
pub use intent_bundles::{
    magic_scheduled_base_intent, schedule as schedule_transactions,
};
pub use magic_sys::init_magic_sys;
pub use magicblock_magic_program_api::*;
pub use outbox_intent::{
    outbox_intent_bundles,
    process_scheduled_commit_sent::{register_scheduled_commit_sent, SentCommit},
};
pub use schedule_transactions::transaction_scheduler::TransactionScheduler;
pub use utils::instruction_utils;
