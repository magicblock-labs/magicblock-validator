use std::{cell::RefCell, collections::VecDeque};

use crate::args::TaskRequest;

#[derive(Default, Debug)]
pub struct ExecutionTlsStash {
    tasks: VecDeque<TaskRequest>,
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

    pub fn clear() {
        EXECUTION_TLS_STASH.with_borrow_mut(|stash| {
            stash.tasks.clear();
            stash.intents.clear();
        })
    }
}
