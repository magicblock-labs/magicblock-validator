use std::{
    collections::{hash_map::Entry, HashMap, LinkedList, VecDeque},
    sync::{Arc, Mutex},
};

use magicblock_program::magic_scheduled_l1_message::{
    MagicL1Message, ScheduledL1Message,
};
use magicblock_rpc_client::MagicblockRpcClient;
use solana_pubkey::Pubkey;
use tokio::sync::mpsc::{error::TryRecvError, Receiver};

use crate::commit_scheduler::{
    commit_scheduler_inner::CommitSchedulerInner, db::DB, Error,
};
const POISONED_INNER_MSG: &str = "Mutex on CommitSchedulerInner is poisoned.";

pub(crate) struct CommitSchedulerWorker<D: DB> {
    db: Arc<D>,
    rpc_client: MagicblockRpcClient,
    receiver: Receiver<ScheduledL1Message>,

    inner: Arc<Mutex<CommitSchedulerInner>>,
}

impl<D: DB> CommitSchedulerWorker<D> {
    pub fn new(
        db: Arc<D>,
        rpc_client: MagicblockRpcClient,
        receiver: Receiver<ScheduledL1Message>,
    ) -> Self {
        Self {
            db,
            rpc_client,
            receiver,
            inner: Arc::new(Mutex::new(CommitSchedulerInner::new())),
        }
    }

    pub async fn start(mut self) {
        loop {
            let l1_message = match self.receiver.try_recv() {
                Ok(val) => val,
                Err(TryRecvError::Empty) => {
                    match self.get_or_wait_next_message().await {
                        Ok(val) => val,
                        Err(err) => panic!(err), // TODO(edwin): handle
                    }
                }
                Err(TryRecvError::Disconnected) => {
                    // TODO(edwin): handle
                    panic!("Asdasd")
                }
            };

            self.handle_message(l1_message).await;
        }
    }

    async fn handle_message(&mut self, l1_message: ScheduledL1Message) {
        if let Some(l1_message) = self
            .inner
            .lock()
            .expect(POISONED_INNER_MSG)
            .schedule(l1_message)
        {
            tokio::spawn(self.execute(l1_message));
        }
    }

    // SchedulerWorker
    // Message arrives
    // Can execute?
    // Yes - execute
    // No - enqueue

    // SchedulerWorker \
    ///             Planner
    //   MessageProcessor /

    /// ScheduledL1Message arrives:
    /// 1. Sent to Scheduler
    /// 2. Scheduler sents to SchedulerWorker
    /// 3. SchedulerWorker checks Sche
    async fn execute(&self, l1_message: ScheduledL1Message) {
        todo!()
    }

    /// Return [`ScheduledL1Message`] from DB, otherwise waits on channel
    async fn get_or_wait_next_message(
        &mut self,
    ) -> Result<ScheduledL1Message, Error> {
        // TODO: expensive to fetch 1 by 1, implement fetching multiple. Could use static?
        if let Some(l1_message) = self.db.pop_l1_message().await? {
            Ok(l1_message)
        } else {
            if let Some(val) = self.receiver.recv().await {
                Ok(val)
            } else {
                Err(Error::ChannelClosed)
            }
        }
    }
}
