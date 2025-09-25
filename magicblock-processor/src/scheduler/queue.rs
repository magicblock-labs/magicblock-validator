use std::collections::VecDeque;

use magicblock_core::link::transactions::ProcessableTransaction;

pub struct BlockedTransactionsQueue {
    inner: Vec<VecDeque<ProcessableTransaction>>,
}

impl BlockedTransactionsQueue {
    pub(crate) fn new(workers: u32) -> Self {
        let inner = (0..workers).map(|_| VecDeque::new()).collect();
        Self { inner }
    }
}
