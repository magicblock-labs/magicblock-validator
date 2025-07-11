use std::{
    collections::{btree_map::Entry, HashMap, LinkedList, VecDeque},
    sync::Arc,
};

use magicblock_program::magic_scheduled_l1_message::{
    MagicL1Message, ScheduledL1Message,
};
use magicblock_rpc_client::MagicblockRpcClient;
use solana_pubkey::Pubkey;
use tokio::sync::mpsc::{error::TryRecvError, Receiver, Sender};

use crate::commit_scheduler::{db::DB, Error};

type MessageID = u64;

struct MessageMeta {
    num_keys: usize,
    message: ScheduledL1Message,
}

/// A scheduler that ensures mutually exclusive access to pubkeys across messages
///
/// # Data Structures
///
/// 1. `executing`: Tracks currently running messages and their locked pubkeys
///    - Key: MessageID
///    - Value: Vec of locked Pubkeys
///
/// 2. `blocked_keys`: Maintains FIFO queues of messages waiting for each pubkey
///    - Key: Pubkey
///    - Value: Queue of MessageIDs in arrival order
///
/// 3. `blocked_messages`: Stores metadata for all blocked messages
///    - Key: MessageID
///    - Value: Message metadata including original message
///
/// # Scheduling Logic
///
/// 1. On message arrival:
///    - Check if any required pubkey exists in `blocked_keys`
///    - If conflicted: Add message to all relevant pubkey queues
///    - Else: Start executing immediately
///
/// 2. On message completion:
///    - Remove message from all pubkey queues
///    - For each modified queue:
///      * If front message now has all its pubkeys available:
///        - Move from `blocked_messages` to `executing`
///
/// (1) Assume t1:
/// executing: [a1, a2, a3] [b1, b2, b3]
/// blocked:   [a1,         b1]
/// arriving:  [a1,     a3]
///
/// t2:
/// executing: [b1, b2, b3]
/// blocked:   [a1,         b1]
/// CAN't be executed - [a1, a3], since [a1, b3] needs to be sent first, it has earlier state
///
/// (2) Assume:
/// executing:         [a1, a2, a3]
/// blocked:      [c1, a1]
/// arriving: [c2, c1]
/// [c2, c1] - Even there's no overlaps with executing
/// we can't be proceed since blocked one has [c1] that has to be executed first
pub(crate) struct CommitSchedulerWorker<D: DB> {
    db: Arc<D>,
    rpc_client: MagicblockRpcClient,
    receiver: Receiver<ScheduledL1Message>,

    executing: HashMap<MessageID, Vec<Pubkey>>,
    blocked_keys: HashMap<Pubkey, VecDeque<MessageID>>,
    blocked_messages: HashMap<MessageID, MessageMeta>,
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
            executing: HashMap::new(),
            blocked_keys: HashMap::new(),
            blocked_messages: HashMap::new(),
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
        let message_id = l1_message.id;
        let accounts = match &l1_message.l1_message {
            MagicL1Message::L1Actions(val) => todo!(),
            MagicL1Message::Commit(val) => val.get_committed_accounts(),
            MagicL1Message::CommitAndUndelegate(val) => {
                val.get_committed_accounts()
            }
        };
        let pubkeys = accounts
            .iter()
            .map(|account| *account.pubkey)
            .collect::<Vec<Pubkey>>();

        if Self::process_conflicting(
            message_id,
            &pubkeys,
            &mut self.blocked_keys,
        ) {
            self.blocked_messages.insert(
                message_id,
                MessageMeta {
                    num_keys: pubkeys.len(),
                    message: l1_message,
                },
            );
        } else {
            // Can start to execute
            self.executing.insert(message_id, pubkeys);
            tokio::spawn(self.execute(l1_message));
        }
    }

    fn process_conflicting(
        message_id: MessageID,
        pubkeys: &[Pubkey],
        blocked_keys: &mut HashMap<Pubkey, VecDeque<MessageID>>,
    ) -> bool {
        pubkeys.iter().any(|pubkey| {
            blocked_keys
                .entry(*pubkey)
                .or_default()
                .push_back(message_id);

            // If had values before - conflicting
            blocked_keys[pubkey].len() > 1
        })
    }

    fn complete_message(&mut self, message_id: MessageID) {
        // Release data for completed message
        let pubkeys = self.executing.remove(&message_id).expect("bug");
        for pubkey in pubkeys {
            let mut entry = match self.blocked_keys.entry(pubkey) {
                Entry::Vacant(_) => panic!("bug"), // TODO(edwin): improve?,
                Entry::Occupied(entry) => entry,
            };
            let blocked_messages: &mut VecDeque<MessageID> = entry.get_mut();
            assert_eq!(
                message_id,
                blocked_messages.pop_front().expect("bug"),
                "bug"
            );

            if blocked_messages.is_empty() {
                entry.remove()
            }
        }

        let mut asd: HashMap<MessageID, u32> = HashMap::new();
        self.blocked_keys.iter().for_each(|(pubkey, queue)| {
            let message_id = queue.front().expect("bug");
            *asd.entry(*message_id).or_default() += 1;
        });

        let mut can_execute = Vec::new();
        for (message_id, free_keys) in asd {
            if self
                .blocked_messages
                .get(&message_id)
                .expect("bug")
                .num_keys
                == free_keys
            {
                can_execute
                    .push(self.blocked_messages.remove(&message_id).unwrap());
                // TODO(edwin): update executing
            }
        }
    }

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
