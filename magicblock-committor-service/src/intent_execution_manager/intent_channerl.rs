use std::sync::Arc;
use tokio::sync::mpsc;
use magicblock_program::magic_scheduled_base_intent::ScheduledIntentBundle;
use crate::intent_execution_manager::db::DB;

pub struct Sender<D: DB> {
    db: Arc<D>,
    sender: mpsc::Sender<ScheduledIntentBundle>
}