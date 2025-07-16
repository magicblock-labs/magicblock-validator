use std::collections::{hash_map::Entry, HashMap, VecDeque};

use magicblock_program::magic_scheduled_l1_message::{
    MagicL1Message, ScheduledL1Message,
};
use solana_pubkey::Pubkey;

use crate::utils::ScheduledMessageExt;

pub(crate) const POISONED_INNER_MSG: &str =
    "Mutex on CommitSchedulerInner is poisoned.";

type MessageID = u64;
struct MessageMeta {
    num_keys: usize,
    message: ScheduledL1Message,
}

/// A scheduler that ensures mutually exclusive access to pubkeys across messages
///
/// # Data Structures
///
/// 1. `blocked_keys`: Maintains FIFO queues of messages waiting for each pubkey
///    - Key: Pubkey
///    - Value: Queue of MessageIDs in arrival order
///
/// 2. `blocked_messages`: Stores metadata for all blocked messages
///    - Key: MessageID
///    - Value: Message metadata including original message
///
/// # Scheduling Logic
///
/// 1. On message arrival:
///     - Check if any required pubkey exists in `blocked_keys`
///     - If conflicted: Add message to all relevant pubkey queues
///     - Else: Start executing immediately
///
/// 2. On message completion:
///     - Pop 1st el-t from corresponding to Message `blocked_keys` queues,
///       Note: `blocked_keys[msg.keys]` == msg.id
///     - This moves forward other messages that were blocked by this one.
///
/// 3. On popping next message to be executed:
///     - Find the first message in `blocked_messages` which
///       has all of its pubkeys unblocked,
///       i.e they are first at corresponding queues
///
/// Some examples/edge cases:
/// (1) Assume `t1`:
/// executing: `[a1, a2, a3] [b1, b2, b3]` - 1
/// blocked:   `[a1,         b1]` - 2
/// arriving:  `[a1,     a3]` - 3
///
/// `t2`:
/// executing: `[b1, b2, b3]`
/// blocked:   `[a1,         b1]`
/// `[a1, a3]` - CAN't be executed, since `[a1, b1]` needs to be sent first, it has earlier state.
///
/// (2) Assume:
/// executing:         `[a1, a2, a3]`
/// blocked:      `[c1, a1]`
/// arriving: `[c2, c1]`
/// `[c2, c1]` - Even there's no overlaps with executing
/// we can't proceed since blocked message has [c1] that has to be executed first
pub(crate) struct CommitSchedulerInner {
    blocked_keys: HashMap<Pubkey, VecDeque<MessageID>>,
    blocked_messages: HashMap<MessageID, MessageMeta>,
}

impl CommitSchedulerInner {
    pub fn new() -> Self {
        Self {
            blocked_keys: HashMap::new(),
            blocked_messages: HashMap::new(),
        }
    }

    /// Returns [`ScheduledL1Message`] if message can be executed,
    /// otherwise consumes it and enqueues
    pub fn schedule(
        &mut self,
        l1_message: ScheduledL1Message,
    ) -> Option<ScheduledL1Message> {
        let message_id = l1_message.id;
        let Some(accounts) = l1_message.get_committed_accounts() else {
            return Some(l1_message);
        };

        let pubkeys = accounts
            .iter()
            .map(|account| *account.pubkey)
            .collect::<Vec<Pubkey>>();

        let (entries, is_conflicting) =
            Self::find_conflicting_entries(&pubkeys, &mut self.blocked_keys);
        // In any case block the corresponding accounts
        entries
            .into_iter()
            .for_each(|entry| entry.or_default().push_back(message_id));
        if is_conflicting {
            // Enqueue incoming message
            self.blocked_messages.insert(
                message_id,
                MessageMeta {
                    num_keys: pubkeys.len(),
                    message: l1_message,
                },
            );
            None
        } else {
            Some(l1_message)
        }
    }

    /// Completes Message, cleaning up data after itself and allowing Messages to move forward
    /// Note: this shall be called on executing messages to finilize their execution.
    /// Calling on incorrect `pubkyes` set will result in panic
    pub fn complete(&mut self, l1_message: &ScheduledL1Message) {
        // Release data for completed message
        let message_id = l1_message.id;
        let Some(pubkeys) = l1_message.get_committed_pubkeys() else {
            // This means L1Action, it doesn't have to be scheduled
            return;
        };

        let (entries, _) =
            Self::find_conflicting_entries(&pubkeys, &mut self.blocked_keys);
        entries.into_iter().for_each(|entry| {
            let mut occupied = match entry {
                Entry::Vacant(_) => unreachable!("Invariant: queue for conflicting tasks shall exist"),
                Entry::Occupied(value) => value
            };

            let blocked_messages: &mut VecDeque<MessageID> = occupied.get_mut();
            assert_eq!(
                message_id,
                blocked_messages.pop_front().expect("Invariant: if message executing, queue for each account is non-empty"),
                "Invariant: executing message must be first at qeueue"
            );

            if blocked_messages.is_empty() {
                occupied.remove();
            }
        });
    }

    // Returns [`ScheduledL1Message`] that can be executed
    pub fn pop_next_scheduled_message(&mut self) -> Option<ScheduledL1Message> {
        // TODO(edwin): optimize. Create counter im MessageMeta & update
        let mut execute_candidates: HashMap<MessageID, usize> = HashMap::new();
        self.blocked_keys.iter().for_each(|(pubkey, queue)| {
            let message_id = queue
                .front()
                .expect("Invariant: we maintain ony non-empty queues");
            *execute_candidates.entry(*message_id).or_default() += 1;
        });

        let candidate =
            self.blocked_messages.iter().find_map(|(message_id, meta)| {
                if execute_candidates.get(message_id).expect(
                    "Invariant: blocked messages are always in candidates",
                ) == &meta.num_keys
                {
                    Some(message_id)
                } else {
                    None
                }
            });

        if let Some(next) = candidate {
            Some(self.blocked_messages.remove(next).unwrap().message)
        } else {
            None
        }
    }

    fn find_conflicting_entries<'a>(
        pubkeys: &[Pubkey],
        blocked_keys: &'a mut HashMap<Pubkey, VecDeque<MessageID>>,
    ) -> (Vec<Entry<'a, Pubkey, VecDeque<MessageID>>>, bool) {
        let mut is_conflicting = false;
        let entries = pubkeys
            .iter()
            .map(|pubkey| {
                let entry = blocked_keys.entry(*pubkey);

                if is_conflicting {
                    entry
                } else {
                    if let Entry::Occupied(_) = &entry {
                        is_conflicting = true;
                        entry
                    } else {
                        entry
                    }
                }
            })
            .collect();

        (entries, is_conflicting)
    }

    /// Returns number of blocked messages
    /// Note: this doesn't include "executing" messages
    pub fn blocked_messages_len(&self) -> usize {
        self.blocked_messages.len()
    }
}
