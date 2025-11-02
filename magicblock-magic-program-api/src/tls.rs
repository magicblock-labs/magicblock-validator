use std::{cell::RefCell, collections::VecDeque};

use crate::args::TaskRequest;

#[derive(Default, Debug)]
pub struct ExecutionTlsStash {
    pub tasks: VecDeque<TaskRequest>,
    // TODO(bmuddha/taco-paco): intents should go in here
    pub intents: VecDeque<()>,
}

thread_local! {
    pub static EXECUTION_TLS_STASH: RefCell<ExecutionTlsStash> = RefCell::default();
}

impl ExecutionTlsStash {
    pub fn clear(&mut self) {
        self.tasks.clear();
        self.intents.clear();
    }
}
