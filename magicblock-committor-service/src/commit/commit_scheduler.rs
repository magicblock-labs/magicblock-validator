use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc::error::{TryRecvError, TrySendError};
use tokio::select;
use tokio::sync::mpsc::{channel, Receiver, Sender};
use tokio::sync::mpsc::error::SendError;
use magicblock_program::magic_scheduled_l1_message::ScheduledL1Message;

pub struct CommitScheduler {
    queue: VecDeque<ScheduledL1Message>,
    sender: Sender<ScheduledL1Message>,
}

impl CommitScheduler {
    pub fn new() -> Self {
        // TODO: define
        let (sender, receiver) = channel(1000);
        tokio::spawn(Self::start(receiver));

        Self {
            queue: VecDeque::default(),
            sender
        }
    }

    async fn start(mut l1_message_receiver: Receiver<ScheduledL1Message>, db_flag: Arc<AtomicBool>) {
        // scheduler
        // accepts messages
        // if no commits we shall be idle
        loop {
            let message = match l1_message_receiver.try_recv() {
                Ok(val) => {
                    val
                }
                Err(TryRecvError::Empty) => {
                    if let Ok(val) = Self::get_next_message(&mut l1_message_receiver, &db_flag).await {
                        val
                    } else {
                        // TODO(edwin): handle
                        panic!("Asdasd")
                    }
                }
                Err(TryRecvError::Disconnected) => {
                    // TODO(edwin): handle
                    panic!("Asdasd")
                },
            };

            // send and shit
            todo!()
        }

        while let Some(l1_messages) = l1_message_receiver.recv().await {

        }
    }

    async fn get_next_message(l1_message_receiver: &mut Receiver<ScheduledL1Message>, db_flag: &AtomicBool) -> Result<ScheduledL1Message, Error> {
        if db_flag.load(Ordering::Relaxed) {
            // TODO: expensive to fetch 1 by 1, implement fetching multiple. Could use static?
            Self::get_message_from_db().await
        } else {
            if let Some(val) = l1_message_receiver.recv().await {
                Ok(val)
            } else {
                Err(Error::ChannelClosed)
            }
        }
    }

    // TODO(edwin)
    async fn get_message_from_db() -> Result<ScheduledL1Message, Error> {
        todo!()
    }

    pub async fn schedule(&self, l1_messages: Vec<ScheduledL1Message>) -> Result<(), Error>{
        for el in l1_messages {
            let err = if let Err(err) = self.sender.try_send(el) {
                err
            } else {
                continue;
            };

            if matches!(err, TrySendError::Closed(_)) {
                return Err(Error::ChannelClosed)
            }

        }

        Ok(())
    }
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("Channel was closed")]
    ChannelClosed
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
///
///
/// 1.