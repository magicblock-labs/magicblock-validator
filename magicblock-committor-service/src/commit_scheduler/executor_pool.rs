use std::future::Future;

use tokio::{
    sync::{Semaphore, SemaphorePermit},
    task::JoinHandle,
};

type MessageExecutorResult = ();

pub(crate) struct MessageExecutorsPool {
    limit: u8,
    semaphore: Semaphore,
    handles: Vec<JoinHandle<MessageExecutorResult>>,
}

impl MessageExecutorsPool {
    pub fn new(limit: u8) -> Self {
        Self {
            limit,
            semaphore: Semaphore::new(limit as usize),
            handles: vec![],
        }
    }

    pub async fn execute<O, T: Future<Output = O>>(
        &self,
        f: impl FnOnce(SemaphorePermit) -> T,
    ) {
        let permit = self.semaphore.acquire().await.expect("asd");
        f(permit).await
    }

    pub async fn get_worker_permit(&self) -> SemaphorePermit {
        let permit = self.semaphore.acquire().await.unwrap();
        permit
    }
}

// TODO: how executiong works?
// case - No available worker
// We can't process any messages - waiting

// Say worker finished
// Messages still blocked by each other
// We move to get message from channel
// We stuck
// If we get worker without

// Flow:
// 1. check if there's available message to be executed
// 2. Fetch it and wait for available worker to execute it

// If no messages workers are idle
// If more tham one free, then we will launch woker and pick another
// on next iteration
