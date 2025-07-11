mod commit_scheduler_worker;
mod db;

use std::sync::Arc;

use magicblock_program::magic_scheduled_l1_message::ScheduledL1Message;
use magicblock_rpc_client::MagicblockRpcClient;
use tokio::sync::mpsc::{channel, error::TrySendError, Sender};

use crate::commit_scheduler::{
    commit_scheduler_worker::CommitSchedulerWorker, db::DB,
};

pub struct CommitScheduler<D: DB> {
    db: Arc<D>,
    sender: Sender<ScheduledL1Message>,
}

impl<D: DB> CommitScheduler<D> {
    pub fn new(rpc_client: MagicblockRpcClient, db: D) -> Self {
        let db = Arc::new(db);
        let (sender, receiver) = channel(1000);

        // TODO(edwin): add concellation logic
        let worker =
            CommitSchedulerWorker::new(db.clone(), rpc_client, receiver);
        tokio::spawn(worker.start());

        Self { db, sender }
    }

    /// Schedules [`ScheduledL1Message`] message to be executed
    /// In case the channel is full we write message to DB
    /// Messages will be extracted and handled in the [`CommitSchedulerWorker`]
    pub async fn schedule(
        &self,
        l1_messages: Vec<ScheduledL1Message>,
    ) -> Result<(), Error> {
        for el in l1_messages {
            let err = if let Err(err) = self.sender.try_send(el) {
                err
            } else {
                continue;
            };

            if matches!(err, TrySendError::Closed(_)) {
                Err(Error::ChannelClosed)
            } else {
                self.db
                    .store_l1_messages(l1_messages)
                    .await
                    .map_err(Error::from)
            }?
        }

        Ok(())
    }
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("Channel was closed")]
    ChannelClosed,
    #[error("DBError: {0}")]
    DBError(#[from] db::Error),
}

/// ideal system:
///
// Service keeps accepting messages
// once there's a full channel in order not to stall or overload RAM
// we write to

// Having message service batches in optimal way

/// WE NEED:
// - Split into proper Commitable chunks
// -

/// We insert into scheduler and then figure out how to optimally split messages
// or we split messages and then try to commit specific chunks?

// we write to channel it becom3s full
// we need to write to db
// Who will

// TODO Scheduler also return revicer chammel that will receive
// (commit_id, signature)s that it sent. Single worker in [`RemoteScheduledCommitsProcessor`]
// can receive them and hande them txs and sucj

// after we flagged that items in db
// next sends can't fo to queue, since that will break an order
// they need to go to db.

// Our loop

/// Design:
/// Let it be a general service
/// Gets directly commits from Processor, then
fn useless() {}
