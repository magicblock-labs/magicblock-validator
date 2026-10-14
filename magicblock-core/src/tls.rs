use std::{cell::RefCell, collections::VecDeque, mem};

use magicblock_magic_program_api::args::TaskRequest;
use solana_pubkey::Pubkey;

use crate::intent::ScheduledIntentBundle;

#[derive(Default, Debug)]
pub struct ExecutionTlsStash {
    tasks: VecDeque<TaskRequest>,
    newly_created_magic_atas: VecDeque<Pubkey>,
    intents: Vec<ScheduledIntentBundle>,
}

thread_local! {
    static EXECUTION_TLS_STASH: RefCell<ExecutionTlsStash> = RefCell::default();
}

impl ExecutionTlsStash {
    pub fn register_task(task: TaskRequest) {
        EXECUTION_TLS_STASH
            .with_borrow_mut(|stash| stash.tasks.push_back(task));
    }

    pub fn next_task() -> Option<TaskRequest> {
        EXECUTION_TLS_STASH.with_borrow_mut(|stash| stash.tasks.pop_front())
    }

    pub fn register_newly_created_magic_ata(pubkey: Pubkey) {
        EXECUTION_TLS_STASH.with_borrow_mut(|stash| {
            stash.newly_created_magic_atas.push_back(pubkey)
        });
    }

    pub fn pop_newly_created_magic_ata() -> Option<Pubkey> {
        EXECUTION_TLS_STASH
            .with_borrow_mut(|stash| stash.newly_created_magic_atas.pop_front())
    }

    /// Stages accepted intents until the enclosing transaction commits.
    pub fn register_scheduled_intent_bundles(
        intents: Vec<ScheduledIntentBundle>,
    ) {
        EXECUTION_TLS_STASH.with_borrow_mut(|stash| {
            if stash.intents.is_empty() {
                stash.intents = intents;
            } else {
                stash.intents.extend(intents);
            }
        });
    }

    pub fn take_scheduled_intent_bundles() -> Vec<ScheduledIntentBundle> {
        EXECUTION_TLS_STASH
            .with_borrow_mut(|stash| mem::take(&mut stash.intents))
    }

    pub fn clear() {
        EXECUTION_TLS_STASH.with_borrow_mut(|stash| {
            stash.tasks.clear();
            stash.newly_created_magic_atas.clear();
            stash.intents.clear();
        })
    }
}
