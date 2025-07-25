use std::collections::{hash_map::Entry, HashMap, VecDeque};

use magicblock_program::magic_scheduled_l1_message::ScheduledL1Message;
use solana_pubkey::Pubkey;

use crate::{types::ScheduledL1MessageWrapper, utils::ScheduledMessageExt};

pub(crate) const POISONED_INNER_MSG: &str =
    "Mutex on CommitSchedulerInner is poisoned.";

type MessageID = u64;
struct MessageMeta {
    num_keys: usize,
    message: ScheduledL1MessageWrapper,
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
        l1_message: ScheduledL1MessageWrapper,
    ) -> Option<ScheduledL1MessageWrapper> {
        let message_id = l1_message.scheduled_l1_message.id;
        let Some(pubkeys) =
            l1_message.scheduled_l1_message.get_committed_pubkeys()
        else {
            return Some(l1_message);
        };

        // Check if there are any conflicting keys
        let is_conflicting = pubkeys
            .iter()
            .any(|pubkey| self.blocked_keys.contains_key(pubkey));
        // In any case block the corresponding accounts
        pubkeys.iter().for_each(|pubkey| {
            self.blocked_keys
                .entry(*pubkey)
                .or_default()
                .push_back(message_id)
        });

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
    /// NOTE: This doesn't unblock message, hence Self::messages_blocked will return old value.
    /// NOTE: this shall be called on executing messages to finilize their execution.
    /// Calling on incorrect `pubkyes` set will result in panic
    pub fn complete(&mut self, l1_message: &ScheduledL1Message) {
        // Release data for completed message
        let message_id = l1_message.id;
        let Some(pubkeys) = l1_message.get_committed_pubkeys() else {
            // This means L1Action, it doesn't have to be scheduled
            return;
        };

        pubkeys
            .iter()
            .for_each(|pubkey| {
            let mut occupied = match self.blocked_keys.entry(*pubkey) {
                Entry::Vacant(_) => unreachable!("Invariant: queue for conflicting tasks shall exist"),
                Entry::Occupied(value) => value
            };

            let blocked_messages: &mut VecDeque<MessageID> = occupied.get_mut();
            let front = blocked_messages.pop_front();
            assert_eq!(
                message_id,
                front.expect("Invariant: if message executing, queue for each account is non-empty"),
                "Invariant: executing message must be first at qeueue"
            );

            if blocked_messages.is_empty() {
                occupied.remove();
            }
        });
    }

    // Returns [`ScheduledL1Message`] that can be executed
    pub fn pop_next_scheduled_message(
        &mut self,
    ) -> Option<ScheduledL1MessageWrapper> {
        // TODO(edwin): optimize. Create counter im MessageMeta & update
        let mut execute_candidates: HashMap<MessageID, usize> = HashMap::new();
        self.blocked_keys.iter().for_each(|(_, queue)| {
            let message_id = queue
                .front()
                .expect("Invariant: we maintain ony non-empty queues");
            *execute_candidates.entry(*message_id).or_default() += 1;
        });

        // NOTE:
        // Not all self.blocked_messages would be in execute_candidates
        // t1:
        // 1: [a, b]
        // 2: [a, b]
        // 3: [b]
        // t2:
        // 1: [a, b] - completed
        // 2: [a, b]
        // 3: [b]
        // now 3 is in blocked messages but not in execute candidate
        // NOTE:
        // Other way around is also true, since execute_candidates also include
        // currently executing messages
        let candidate =
            execute_candidates.iter().find_map(|(id, ready_keys)| {
                if let Some(candidate) = self.blocked_messages.get(id) {
                    if candidate.num_keys.eq(ready_keys) {
                        Some(id)
                    } else {
                        // Not enough keys are ready
                        None
                    }
                } else {
                    // This means that this message id is currently executing & not blocked
                    None
                }
            });

        if let Some(next) = candidate {
            Some(self.blocked_messages.remove(next).unwrap().message)
        } else {
            None
        }
    }

    /// Returns number of blocked messages
    /// Note: this doesn't include "executing" messages
    pub fn messages_blocked(&self) -> usize {
        self.blocked_messages.len()
    }
}

/// Set of simple tests
#[cfg(test)]
mod simple_test {
    use solana_pubkey::pubkey;

    use super::*;

    #[test]
    fn test_empty_scheduler() {
        let mut scheduler = CommitSchedulerInner::new();
        assert_eq!(scheduler.messages_blocked(), 0);
        assert!(scheduler.pop_next_scheduled_message().is_none());
    }

    /// Ensure messages with non-conflicting set of keys can run in parallel
    #[test]
    fn test_non_conflicting_messages() {
        let mut scheduler = CommitSchedulerInner::new();
        let msg1 = create_test_message(
            1,
            &[pubkey!("1111111111111111111111111111111111111111111")],
        );
        let msg2 = create_test_message(
            2,
            &[pubkey!("22222222222222222222222222222222222222222222")],
        );

        // First message should execute immediately
        assert!(scheduler.schedule(msg1.clone()).is_some());
        // Second message should also execute immediately
        assert!(scheduler.schedule(msg2.clone()).is_some());
        // No messages are blocked
        assert_eq!(scheduler.messages_blocked(), 0);
    }

    /// Ensure messages conflicting messages get blocked
    #[test]
    fn test_conflicting_messages() {
        const NUM_MESSAGES: u64 = 10;

        let mut scheduler = CommitSchedulerInner::new();
        let pubkey = pubkey!("1111111111111111111111111111111111111111111");
        let msg1 = create_test_message(1, &[pubkey]);

        // First message executes immediately
        assert!(scheduler.schedule(msg1).is_some());
        for id in 2..=NUM_MESSAGES {
            let msg = create_test_message(id, &[pubkey]);
            // Message gets blocked
            assert!(scheduler.schedule(msg).is_none());
        }

        // 1 message executing, NUM_MESSAGES - 1 are blocked
        assert_eq!(scheduler.messages_blocked() as u64, NUM_MESSAGES - 1);
    }
}

/// Set of simple completion tests
#[cfg(test)]
mod completion_simple_test {
    use solana_pubkey::pubkey;

    use super::*;

    #[test]
    fn test_completion_unblocks_messages() {
        let mut scheduler = CommitSchedulerInner::new();
        let pubkey = pubkey!("1111111111111111111111111111111111111111111");
        let msg1 = create_test_message(1, &[pubkey]);
        let msg2 = create_test_message(2, &[pubkey]);

        // First message executes immediately
        let executed = scheduler.schedule(msg1.clone()).unwrap();
        // Second message gets blocked
        assert!(scheduler.schedule(msg2.clone()).is_none());
        assert_eq!(scheduler.messages_blocked(), 1);

        // Complete first message
        scheduler.complete(&executed.scheduled_l1_message);

        let next = scheduler.pop_next_scheduled_message().unwrap();
        assert_eq!(next, msg2);
        assert_eq!(scheduler.messages_blocked(), 0);
    }

    #[test]
    fn test_multiple_blocked_messages() {
        let mut scheduler = CommitSchedulerInner::new();
        let pubkey = pubkey!("1111111111111111111111111111111111111111111");
        let msg1 = create_test_message(1, &[pubkey]);
        let msg2 = create_test_message(2, &[pubkey]);
        let msg3 = create_test_message(3, &[pubkey]);

        // First message executes immediately
        let executed = scheduler.schedule(msg1.clone()).unwrap();
        // Others get blocked
        assert!(scheduler.schedule(msg2.clone()).is_none());
        assert!(scheduler.schedule(msg3.clone()).is_none());
        assert_eq!(scheduler.messages_blocked(), 2);

        // Complete first message
        scheduler.complete(&executed.scheduled_l1_message);

        // Second message should now be available
        let expected_msg2 = scheduler.pop_next_scheduled_message().unwrap();
        assert_eq!(expected_msg2, msg2);
        assert_eq!(scheduler.messages_blocked(), 1);

        // Complete second message
        scheduler.complete(&expected_msg2.scheduled_l1_message);

        // Third message should now be available
        let expected_msg3 = scheduler.pop_next_scheduled_message().unwrap();
        assert_eq!(expected_msg3, msg3);
        assert_eq!(scheduler.messages_blocked(), 0);
    }
}

#[cfg(test)]
mod complex_blocking_test {
    use solana_pubkey::pubkey;

    use super::*;

    /// Case:
    /// executing: `[a1, a2, a3] [b1, b2, b3]` - 1
    /// blocked:   `[a1,         b1]` - 2
    /// arriving:  `[a1,     a3]` - 3
    #[test]
    fn test_edge_case_1_earlier_message_blocks_later_overlapping() {
        let mut scheduler = CommitSchedulerInner::new();
        let a1 = pubkey!("1111111111111111111111111111111111111111111");
        let a2 = pubkey!("21111111111111111111111111111111111111111111");
        let a3 = pubkey!("31111111111111111111111111111111111111111111");
        let b1 = pubkey!("41111111111111111111111111111111111111111111");
        let b2 = pubkey!("51111111111111111111111111111111111111111111");
        let b3 = pubkey!("61111111111111111111111111111111111111111111");

        // Message 1: [a1, a2, a3]
        let msg1_keys = vec![a1, a2, a3];
        let msg1 = create_test_message(1, &msg1_keys);
        assert!(scheduler.schedule(msg1.clone()).is_some());
        assert_eq!(scheduler.messages_blocked(), 0);

        // Message 2:  [b1, b2, b3]
        let msg2_keys = vec![b1, b2, b3];
        let msg2 = create_test_message(2, &msg2_keys);
        assert!(scheduler.schedule(msg2.clone()).is_some());
        assert_eq!(scheduler.messages_blocked(), 0);

        // Message 3: [a1, b1] - blocked by msg1 & msg2
        let msg3_keys = vec![a1, b1];
        let msg3 = create_test_message(3, &msg3_keys);
        assert!(scheduler.schedule(msg3.clone()).is_none());
        assert_eq!(scheduler.messages_blocked(), 1);

        // Message 4: [a1, a3] - blocked by msg1 & msg3
        let msg4_keys = vec![a1, a3];
        let msg4 = create_test_message(4, &msg4_keys);
        assert!(scheduler.schedule(msg4.clone()).is_none());
        assert_eq!(scheduler.messages_blocked(), 2);

        // Complete msg1
        scheduler.complete(&msg1.scheduled_l1_message);
        // None of the messages can execute yet
        // msg3 is blocked msg2
        // msg4 is blocked by msg3
        assert!(scheduler.pop_next_scheduled_message().is_none());

        // Complete msg2
        scheduler.complete(&msg2.scheduled_l1_message);
        // Now msg3 is unblocked
        let next = scheduler.pop_next_scheduled_message().unwrap();
        assert_eq!(next, msg3);
        assert_eq!(scheduler.messages_blocked(), 1);
        // Complete msg3
        scheduler.complete(&next.scheduled_l1_message);

        // Now msg4 should be available
        let next = scheduler.pop_next_scheduled_message().unwrap();
        assert_eq!(next, msg4);
        assert_eq!(scheduler.messages_blocked(), 0);
    }

    /// Case:
    /// executing:         `[a1, a2, a3]`
    /// blocked:      `[c1, a1]`
    /// arriving: `[c2, c1]`
    /// `[c2, c1]` - Even there's no overlaps with executing
    #[test]
    fn test_edge_case_2_indirect_blocking_through_shared_key() {
        let mut scheduler = CommitSchedulerInner::new();
        let a1 = pubkey!("1111111111111111111111111111111111111111111");
        let a2 = pubkey!("21111111111111111111111111111111111111111111");
        let a3 = pubkey!("31111111111111111111111111111111111111111111");
        let c1 = pubkey!("41111111111111111111111111111111111111111111");
        let c2 = pubkey!("51111111111111111111111111111111111111111111");

        // Message 1: [a1, a2, a3] (executing)
        let msg1_keys = vec![a1, a2, a3];
        let msg1 = create_test_message(1, &msg1_keys);

        // Message 2: [c1, a1] (blocked by msg1)
        let msg2_keys = vec![c1, a1];
        let msg2 = create_test_message(2, &msg2_keys);

        // Message 3: [c2, c1] (arriving later)
        let msg3_keys = vec![c2, c1];
        let msg3 = create_test_message(3, &msg3_keys);

        // Schedule msg1 (executes immediately)
        let executed_msg1 = scheduler.schedule(msg1.clone()).unwrap();
        assert_eq!(executed_msg1, msg1);

        // Schedule msg2 (gets blocked)
        assert!(scheduler.schedule(msg2.clone()).is_none());
        assert_eq!(scheduler.messages_blocked(), 1);

        // Schedule msg3 (gets blocked, even though c2 is available)
        assert!(scheduler.schedule(msg3.clone()).is_none());
        assert_eq!(scheduler.messages_blocked(), 2);

        // Complete msg1
        scheduler.complete(&executed_msg1.scheduled_l1_message);

        // Now only msg2 should be available (not msg3)
        let expected_msg2 = scheduler.pop_next_scheduled_message().unwrap();
        assert_eq!(expected_msg2, msg2);
        assert_eq!(scheduler.messages_blocked(), 1);
        // msg 3 still should be blocked
        assert_eq!(scheduler.pop_next_scheduled_message(), None);

        // Complete msg2
        scheduler.complete(&expected_msg2.scheduled_l1_message);

        // Now msg3 should be available
        let expected_msg3 = scheduler.pop_next_scheduled_message().unwrap();
        assert_eq!(expected_msg3, msg3);
        assert_eq!(scheduler.messages_blocked(), 0);
    }

    #[test]
    fn test_complex_contention_scenario() {
        let mut scheduler = CommitSchedulerInner::new();
        let a = pubkey!("1111111111111111111111111111111111111111111");
        let b = pubkey!("21111111111111111111111111111111111111111111");
        let c = pubkey!("31111111111111111111111111111111111111111111");

        // Messages with various key combinations
        let msg1 = create_test_message(1, &[a, b]);
        let msg2 = create_test_message(2, &[a, c]);
        let msg3 = create_test_message(3, &[c]);
        let msg4 = create_test_message(4, &[b]);
        let msg5 = create_test_message(5, &[a]);

        // msg1 executes immediately
        let executed1 = scheduler.schedule(msg1.clone()).unwrap();
        // Others get blocked
        assert!(scheduler.schedule(msg2.clone()).is_none());
        assert!(scheduler.schedule(msg3.clone()).is_none());
        assert!(scheduler.schedule(msg4.clone()).is_none());
        assert!(scheduler.schedule(msg5.clone()).is_none());
        assert_eq!(scheduler.messages_blocked(), 4);

        // Complete msg1
        scheduler.complete(&executed1.scheduled_l1_message);

        // msg2 and msg4 should be available (they don't conflict)
        let next_msgs = [
            scheduler.pop_next_scheduled_message().unwrap(),
            scheduler.pop_next_scheduled_message().unwrap(),
        ];
        assert!(next_msgs.contains(&msg2));
        assert!(next_msgs.contains(&msg4));
        assert_eq!(scheduler.messages_blocked(), 2);

        // Complete msg2
        scheduler.complete(&msg2.scheduled_l1_message);
        // msg2 and msg4 should be available (they don't conflict)
        let next_messages = [
            scheduler.pop_next_scheduled_message().unwrap(),
            scheduler.pop_next_scheduled_message().unwrap(),
        ];
        assert!(next_messages.contains(&msg3));
        assert!(next_messages.contains(&msg5));
        assert_eq!(scheduler.messages_blocked(), 0);
    }
}

#[cfg(test)]
mod edge_cases_test {
    use magicblock_program::magic_scheduled_l1_message::MagicL1Message;
    use solana_pubkey::pubkey;

    use super::*;

    #[test]
    fn test_message_without_pubkeys() {
        let mut scheduler = CommitSchedulerInner::new();
        let mut msg = create_test_message(1, &[]);
        msg.scheduled_l1_message.l1_message = MagicL1Message::L1Actions(vec![]);

        // Should execute immediately since it has no pubkeys
        assert!(scheduler.schedule(msg.clone()).is_some());
        assert_eq!(scheduler.messages_blocked(), 0);
    }

    #[test]
    fn test_completion_without_scheduling() {
        let mut scheduler = CommitSchedulerInner::new();
        let msg = create_test_message(
            1,
            &[pubkey!("11111111111111111111111111111111")],
        );

        // Completing a message that wasn't scheduled should panic
        let result = std::panic::catch_unwind(move || {
            scheduler.complete(&msg.scheduled_l1_message)
        });
        assert!(result.is_err());
    }
}

// Helper function to create test messages
#[cfg(test)]
fn create_test_message(
    id: u64,
    pubkeys: &[Pubkey],
) -> ScheduledL1MessageWrapper {
    use magicblock_program::magic_scheduled_l1_message::{
        CommitType, CommittedAccountV2, MagicL1Message,
    };
    use solana_account::Account;
    use solana_sdk::{hash::Hash, transaction::Transaction};

    use crate::types::TriggerType;

    let mut message = ScheduledL1Message {
        id,
        slot: 0,
        blockhash: Hash::default(),
        action_sent_transaction: Transaction::default(),
        payer: Pubkey::default(),
        l1_message: MagicL1Message::L1Actions(vec![]),
    };

    // Only set pubkeys if provided
    if !pubkeys.is_empty() {
        let committed_accounts = pubkeys
            .iter()
            .map(|&pubkey| CommittedAccountV2 {
                pubkey,
                account: Account::default(),
            })
            .collect();

        message.l1_message =
            MagicL1Message::Commit(CommitType::Standalone(committed_accounts));
    }

    ScheduledL1MessageWrapper {
        scheduled_l1_message: message,
        feepayers: vec![],
        excluded_pubkeys: vec![],
        trigger_type: TriggerType::OffChain,
    }
}
