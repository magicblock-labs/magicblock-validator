use std::{cell::RefCell, collections::VecDeque};

use magicblock_magic_program_api::args::TaskRequest;
use solana_pubkey::Pubkey;

#[derive(Default, Debug)]
pub struct ExecutionTlsStash {
    tasks: VecDeque<TaskRequest>,
    created_rent_pending_atas: VecDeque<Pubkey>,
    recorded_rent_pending_ata_materializations: VecDeque<Pubkey>,
    // TODO(bmuddha/taco-paco): intents should go in here
    intents: VecDeque<()>,
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

    pub fn register_created_rent_pending_ata(pubkey: Pubkey) {
        EXECUTION_TLS_STASH.with_borrow_mut(|stash| {
            stash.created_rent_pending_atas.push_back(pubkey)
        });
    }

    pub fn next_created_rent_pending_ata() -> Option<Pubkey> {
        EXECUTION_TLS_STASH.with_borrow_mut(|stash| {
            stash.created_rent_pending_atas.pop_front()
        })
    }

    pub fn register_recorded_rent_pending_ata_materialization(pubkey: Pubkey) {
        EXECUTION_TLS_STASH.with_borrow_mut(|stash| {
            stash
                .recorded_rent_pending_ata_materializations
                .push_back(pubkey)
        });
    }

    pub fn has_recorded_rent_pending_ata_materialization(
        pubkey: &Pubkey,
    ) -> bool {
        EXECUTION_TLS_STASH.with_borrow(|stash| {
            stash
                .recorded_rent_pending_ata_materializations
                .contains(pubkey)
        })
    }

    pub fn clear() {
        EXECUTION_TLS_STASH.with_borrow_mut(|stash| {
            stash.tasks.clear();
            stash.created_rent_pending_atas.clear();
            stash.recorded_rent_pending_ata_materializations.clear();
            stash.intents.clear();
        })
    }
}
