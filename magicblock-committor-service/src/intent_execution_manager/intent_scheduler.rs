use std::collections::{hash_map::Entry, HashMap, VecDeque};

use log::error;
use magicblock_program::magic_scheduled_base_intent::ScheduledBaseIntent;
use solana_pubkey::Pubkey;
use thiserror::Error;

use crate::types::ScheduledBaseIntentWrapper;

pub(crate) const POISONED_INNER_MSG: &str =
    "Mutex on CommitSchedulerInner is poisoned.";

type IntentID = u64;
struct IntentMeta {
    num_keys: usize,
    intent: ScheduledBaseIntentWrapper,
}

/// A scheduler that ensures mutually exclusive access to pubkeys across intents
///
/// # Data Structures
///
/// 1. `blocked_keys`: Maintains FIFO queues of intents waiting for each pubkey
///    - Key: Pubkey
///    - Value: Queue of IntentIDs in arrival order
///
/// 2. `blocked_intents`: Stores metadata for all blocked intents
///    - Key: IntentID
///    - Value: Intent metadata including original intent
///
/// # Scheduling Logic
///
/// 1. On intent arrival:
///     - Check if any required pubkey exists in `blocked_keys`
///     - If conflicted: Add intent to all relevant pubkey queues
///     - Else: Start executing immediately
///
/// 2. On intent completion:
///     - Pop 1st el-t from corresponding to Intent `blocked_keys` queues,
///       Note: `blocked_keys[msg.keys]` == msg.id
///     - This moves forward other intents that were blocked by this one.
///
/// 3. On popping next intent to be executed:
///     - Find the first intent in `blocked_intents` which
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
/// we can't proceed since blocked intent has [c1] that has to be executed first
/// For tests on those edge-cases refer to complex_blocking_test module
pub(crate) struct IntentScheduler {
    blocked_keys: HashMap<Pubkey, VecDeque<IntentID>>,
    blocked_intents: HashMap<IntentID, IntentMeta>,
}

impl IntentScheduler {
    pub fn new() -> Self {
        Self {
            blocked_keys: HashMap::new(),
            blocked_intents: HashMap::new(),
        }
    }

    /// Returns [`ScheduledBaseIntent`] if intent can be executed,
    /// otherwise consumes it and enqueues
    pub fn schedule(
        &mut self,
        base_intent: ScheduledBaseIntentWrapper,
    ) -> Option<ScheduledBaseIntentWrapper> {
        let intent_id = base_intent.inner.id;

        // To check duplicate scheduling its enough to check:
        // 1. currently blocked
        // 2. currently executing
        if self.blocked_intents.contains_key(&intent_id) {
            // This is critical error as we shouldn't schedule duplicate Intents!
            // this requires investigation
            error!("CRITICAL! Attempt to schedule already scheduled intent!");
            return None;
        }
        let duplicate_executing = self.blocked_keys.iter().any(|(_, queue)| {
            if let Some(executing_id) = queue.front() {
                &intent_id == executing_id
            } else {
                false
            }
        });
        if duplicate_executing {
            // This is critical error as we shouldn't schedule duplicate Intents!
            // this requires investigation
            error!("CRITICAL! Attempt to schedule already scheduled intent!");
            return None;
        }

        let Some(pubkeys) = base_intent.inner.get_committed_pubkeys() else {
            return Some(base_intent);
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
                .push_back(intent_id)
        });

        if is_conflicting {
            // Enqueue incoming intent
            self.blocked_intents.insert(
                intent_id,
                IntentMeta {
                    num_keys: pubkeys.len(),
                    intent: base_intent,
                },
            );
            None
        } else {
            Some(base_intent)
        }
    }

    /// Completes Intent, cleaning up data after itself and allowing Intents to move forward
    /// NOTE: This doesn't unblock intent, hence Self::intents_blocked will return old value.
    /// NOTE: this shall be called on executing intents to finilize their execution.
    pub fn complete(
        &mut self,
        base_intent: &ScheduledBaseIntent,
    ) -> IntentSchedulerResult<()> {
        // Release data for completed intent
        let intent_id = base_intent.id;
        let Some(pubkeys) = base_intent.get_committed_pubkeys() else {
            // This means BaseAction, it doesn't have to be scheduled
            return Ok(());
        };

        if self.blocked_intents.contains_key(&intent_id) {
            return Err(IntentSchedulerError::CompletingBlockedIntentError);
        }

        // All front of queues contain current intent id
        let mut all_front = true;
        // Some of front queues contain intent id
        let mut some_front = false;
        for pubkey in &pubkeys {
            if let Some(blocked_intents) = self.blocked_keys.get(pubkey) {
                // SAFETY: if entry exists it means that queue not empty
                // This is ensured during scheduling as we always insert el-t in the queue
                // Other state is not supposed to be possible
                let front = blocked_intents.front().expect(
                    "Invariant: if entry is occupied, queue is non-empty",
                );
                if front != &intent_id {
                    // This intent isn't executing
                    all_front = false;
                } else {
                    some_front = true;
                }
            } else {
                // This intent isn't executing since queue for it doesn't exist
                all_front = false;
            }
        }

        // Intent is indeed executing - can complete it
        if all_front {
            Ok(())
        } else if some_front {
            // Only some part of pubkeys is executing - corrupted intent
            Err(IntentSchedulerError::CorruptedIntentError)
        } else {
            // Intent was never scheduled before
            Err(IntentSchedulerError::NonScheduledMessageError)
        }?;

        // The last check for corrupted intent
        // Say some keys got account got deleted from intent:
        // We will have all_front = true since number of keys is less than was initially
        let found_in_front = self
            .blocked_keys
            .iter()
            .filter(|(_, queue)| queue.front() == Some(&intent_id))
            .count();
        if found_in_front != pubkeys.len() {
            return Err(IntentSchedulerError::CorruptedIntentError);
        }

        // After all the checks we may safely complete
        pubkeys.iter().for_each(|pubkey| {
            let mut occupied = match self.blocked_keys.entry(*pubkey) {
                Entry::Vacant(_) => {
                    // SAFETY: prior to this we iterated all pubkeys
                    // and ensured that they all exist, so we never will reach this point
                    unreachable!(
                        "entry exists since following was checked beforehand"
                    )
                }
                Entry::Occupied(value) => value,
            };

            let blocked_intents: &mut VecDeque<IntentID> = occupied.get_mut();
            blocked_intents.pop_front();
            if blocked_intents.is_empty() {
                occupied.remove();
            }
        });

        Ok(())
    }

    // Returns [`ScheduledBaseIntent`] that can be executed
    pub fn pop_next_scheduled_intent(
        &mut self,
    ) -> Option<ScheduledBaseIntentWrapper> {
        // TODO(edwin): optimize. Create counter im IntentMeta & update
        let mut execute_candidates: HashMap<IntentID, usize> = HashMap::new();
        self.blocked_keys.iter().for_each(|(_, queue)| {
            // SAFETY: if entry exists it means that queue not empty
            // This is ensured during scheduling as we always insert el-t in the queue
            // Other state is not supposed to be possible
            let intent_id = queue
                .front()
                .expect("Invariant: we maintain ony non-empty queues");
            *execute_candidates.entry(*intent_id).or_default() += 1;
        });

        // NOTE:
        // Not all self.blocked_intents would be in execute_candidates
        // t1:
        // 1: [a, b]
        // 2: [a, b]
        // 3: [b]
        // t2:
        // 1: [a, b] - completed
        // 2: [a, b]
        // 3: [b]
        // now 3 is in blocked intents but not in execute candidate
        // NOTE:
        // Other way around is also true, since execute_candidates also include
        // currently executing intents

        // Find and process the first eligible intent
        execute_candidates.into_iter().find_map(|(id, ready_keys)| {
            match self.blocked_intents.entry(id) {
                Entry::Occupied(entry) => {
                    if entry.get().num_keys == ready_keys {
                        Some(entry.remove().intent)
                    } else {
                        None
                    }
                }
                _ => None,
            }
        })
    }

    /// Returns number of blocked intents
    /// Note: this doesn't include "executing" intents
    pub fn intents_blocked(&self) -> usize {
        self.blocked_intents.len()
    }
}

#[derive(Error, Debug)]
pub enum IntentSchedulerError {
    #[error("Attempt to complete non-scheduled message")]
    NonScheduledMessageError,
    #[error("Attempt to complete corrupted intent")]
    CorruptedIntentError,
    #[error("Attempt to complete blocked message")]
    CompletingBlockedIntentError,
}

pub type IntentSchedulerResult<T, E = IntentSchedulerError> = Result<T, E>;

/// Set of simple tests
#[cfg(test)]
mod simple_test {
    use solana_pubkey::pubkey;

    use super::*;

    #[test]
    fn test_empty_scheduler() {
        let mut scheduler = IntentScheduler::new();
        assert_eq!(scheduler.intents_blocked(), 0);
        assert!(scheduler.pop_next_scheduled_intent().is_none());
    }

    /// Ensure intents with non-conflicting set of keys can run in parallel
    #[test]
    fn test_non_conflicting_intents() {
        let mut scheduler = IntentScheduler::new();
        let msg1 = create_test_intent(
            1,
            &[pubkey!("1111111111111111111111111111111111111111111")],
        );
        let msg2 = create_test_intent(
            2,
            &[pubkey!("22222222222222222222222222222222222222222222")],
        );

        // First intent should execute immediately
        assert!(scheduler.schedule(msg1.clone()).is_some());
        // Second intent should also execute immediately
        assert!(scheduler.schedule(msg2.clone()).is_some());
        // No intents are blocked
        assert_eq!(scheduler.intents_blocked(), 0);
    }

    /// Ensure intents conflicting intents get blocked
    #[test]
    fn test_conflicting_intents() {
        const NUM_INTENTS: u64 = 10;

        let mut scheduler = IntentScheduler::new();
        let pubkey = pubkey!("1111111111111111111111111111111111111111111");
        let msg1 = create_test_intent(1, &[pubkey]);

        // First message executes immediately
        assert!(scheduler.schedule(msg1).is_some());
        for id in 2..=NUM_INTENTS {
            let msg = create_test_intent(id, &[pubkey]);
            // intent gets blocked
            assert!(scheduler.schedule(msg).is_none());
        }

        // 1 intent executing, NUM_INTENTS - 1 are blocked
        assert_eq!(scheduler.intents_blocked() as u64, NUM_INTENTS - 1);
    }
}

/// Set of simple completion tests
#[cfg(test)]
mod completion_simple_test {
    use solana_pubkey::pubkey;

    use super::*;

    #[test]
    fn test_completion_unblocks_intents() {
        let mut scheduler = IntentScheduler::new();
        let pubkey = pubkey!("1111111111111111111111111111111111111111111");
        let msg1 = create_test_intent(1, &[pubkey]);
        let msg2 = create_test_intent(2, &[pubkey]);

        // First intent executes immediately
        let executed = scheduler.schedule(msg1.clone()).unwrap();
        // Second intent gets blocked
        assert!(scheduler.schedule(msg2.clone()).is_none());
        assert_eq!(scheduler.intents_blocked(), 1);

        // Complete first intent
        assert!(scheduler.complete(&executed.inner).is_ok());

        let next = scheduler.pop_next_scheduled_intent().unwrap();
        assert_eq!(next, msg2);
        assert_eq!(scheduler.intents_blocked(), 0);
    }

    #[test]
    fn test_multiple_blocked_intents() {
        let mut scheduler = IntentScheduler::new();
        let pubkey = pubkey!("1111111111111111111111111111111111111111111");
        let msg1 = create_test_intent(1, &[pubkey]);
        let msg2 = create_test_intent(2, &[pubkey]);
        let msg3 = create_test_intent(3, &[pubkey]);

        // First intent executes immediately
        let executed = scheduler.schedule(msg1.clone()).unwrap();
        // Others get blocked
        assert!(scheduler.schedule(msg2.clone()).is_none());
        assert!(scheduler.schedule(msg3.clone()).is_none());
        assert_eq!(scheduler.intents_blocked(), 2);

        // Complete first intent
        assert!(scheduler.complete(&executed.inner).is_ok());

        // Second intent should now be available
        let expected_msg2 = scheduler.pop_next_scheduled_intent().unwrap();
        assert_eq!(expected_msg2, msg2);
        assert_eq!(scheduler.intents_blocked(), 1);

        // Complete second intent
        assert!(scheduler.complete(&expected_msg2.inner).is_ok());

        // Third intent should now be available
        let expected_msg3 = scheduler.pop_next_scheduled_intent().unwrap();
        assert_eq!(expected_msg3, msg3);
        assert_eq!(scheduler.intents_blocked(), 0);
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
    fn test_edge_case_1_earlier_intent_blocks_later_overlapping() {
        let mut scheduler = IntentScheduler::new();
        let a1 = pubkey!("1111111111111111111111111111111111111111111");
        let a2 = pubkey!("21111111111111111111111111111111111111111111");
        let a3 = pubkey!("31111111111111111111111111111111111111111111");
        let b1 = pubkey!("41111111111111111111111111111111111111111111");
        let b2 = pubkey!("51111111111111111111111111111111111111111111");
        let b3 = pubkey!("61111111111111111111111111111111111111111111");

        // intent 1: [a1, a2, a3]
        let msg1_keys = vec![a1, a2, a3];
        let msg1 = create_test_intent(1, &msg1_keys);
        assert!(scheduler.schedule(msg1.clone()).is_some());
        assert_eq!(scheduler.intents_blocked(), 0);

        // intent 2:  [b1, b2, b3]
        let msg2_keys = vec![b1, b2, b3];
        let msg2 = create_test_intent(2, &msg2_keys);
        assert!(scheduler.schedule(msg2.clone()).is_some());
        assert_eq!(scheduler.intents_blocked(), 0);

        // intent 3: [a1, b1] - blocked by msg1 & msg2
        let msg3_keys = vec![a1, b1];
        let msg3 = create_test_intent(3, &msg3_keys);
        assert!(scheduler.schedule(msg3.clone()).is_none());
        assert_eq!(scheduler.intents_blocked(), 1);

        // intent 4: [a1, a3] - blocked by msg1 & msg3
        let msg4_keys = vec![a1, a3];
        let msg4 = create_test_intent(4, &msg4_keys);
        assert!(scheduler.schedule(msg4.clone()).is_none());
        assert_eq!(scheduler.intents_blocked(), 2);

        // Complete msg1
        assert!(scheduler.complete(&msg1.inner).is_ok());
        // None of the intents can execute yet
        // msg3 is blocked msg2
        // msg4 is blocked by msg3
        assert!(scheduler.pop_next_scheduled_intent().is_none());

        // Complete msg2
        assert!(scheduler.complete(&msg2.inner).is_ok());
        // Now msg3 is unblocked
        let next = scheduler.pop_next_scheduled_intent().unwrap();
        assert_eq!(next, msg3);
        assert_eq!(scheduler.intents_blocked(), 1);
        // Complete msg3
        assert!(scheduler.complete(&next.inner).is_ok());

        // Now msg4 should be available
        let next = scheduler.pop_next_scheduled_intent().unwrap();
        assert_eq!(next, msg4);
        assert_eq!(scheduler.intents_blocked(), 0);
    }

    /// Case:
    /// executing:         `[a1, a2, a3]`
    /// blocked:      `[c1, a1]`
    /// arriving: `[c2, c1]`
    /// `[c2, c1]` - Even there's no overlaps with executing
    #[test]
    fn test_edge_case_2_indirect_blocking_through_shared_key() {
        let mut scheduler = IntentScheduler::new();
        let a1 = pubkey!("1111111111111111111111111111111111111111111");
        let a2 = pubkey!("21111111111111111111111111111111111111111111");
        let a3 = pubkey!("31111111111111111111111111111111111111111111");
        let c1 = pubkey!("41111111111111111111111111111111111111111111");
        let c2 = pubkey!("51111111111111111111111111111111111111111111");

        // intent 1: [a1, a2, a3] (executing)
        let msg1_keys = vec![a1, a2, a3];
        let msg1 = create_test_intent(1, &msg1_keys);

        // intent 2: [c1, a1] (blocked by msg1)
        let msg2_keys = vec![c1, a1];
        let msg2 = create_test_intent(2, &msg2_keys);

        // intent 3: [c2, c1] (arriving later)
        let msg3_keys = vec![c2, c1];
        let msg3 = create_test_intent(3, &msg3_keys);

        // Schedule msg1 (executes immediately)
        let executed_msg1 = scheduler.schedule(msg1.clone()).unwrap();
        assert_eq!(executed_msg1, msg1);

        // Schedule msg2 (gets blocked)
        assert!(scheduler.schedule(msg2.clone()).is_none());
        assert_eq!(scheduler.intents_blocked(), 1);

        // Schedule msg3 (gets blocked, even though c2 is available)
        assert!(scheduler.schedule(msg3.clone()).is_none());
        assert_eq!(scheduler.intents_blocked(), 2);

        // Complete msg1
        assert!(scheduler.complete(&executed_msg1.inner).is_ok());

        // Now only msg2 should be available (not msg3)
        let expected_msg2 = scheduler.pop_next_scheduled_intent().unwrap();
        assert_eq!(expected_msg2, msg2);
        assert_eq!(scheduler.intents_blocked(), 1);
        // msg 3 still should be blocked
        assert_eq!(scheduler.pop_next_scheduled_intent(), None);

        // Complete msg2
        assert!(scheduler.complete(&expected_msg2.inner).is_ok());

        // Now msg3 should be available
        let expected_msg3 = scheduler.pop_next_scheduled_intent().unwrap();
        assert_eq!(expected_msg3, msg3);
        assert_eq!(scheduler.intents_blocked(), 0);
    }

    #[test]
    fn test_complex_contention_scenario() {
        let mut scheduler = IntentScheduler::new();
        let a = pubkey!("1111111111111111111111111111111111111111111");
        let b = pubkey!("21111111111111111111111111111111111111111111");
        let c = pubkey!("31111111111111111111111111111111111111111111");

        // intents with various key combinations
        let msg1 = create_test_intent(1, &[a, b]);
        let msg2 = create_test_intent(2, &[a, c]);
        let msg3 = create_test_intent(3, &[c]);
        let msg4 = create_test_intent(4, &[b]);
        let msg5 = create_test_intent(5, &[a]);

        // msg1 executes immediately
        let executed1 = scheduler.schedule(msg1.clone()).unwrap();
        // Others get blocked
        assert!(scheduler.schedule(msg2.clone()).is_none());
        assert!(scheduler.schedule(msg3.clone()).is_none());
        assert!(scheduler.schedule(msg4.clone()).is_none());
        assert!(scheduler.schedule(msg5.clone()).is_none());
        assert_eq!(scheduler.intents_blocked(), 4);

        // Complete msg1
        assert!(scheduler.complete(&executed1.inner).is_ok());

        // msg2 and msg4 should be available (they don't conflict)
        let next_msgs = [
            scheduler.pop_next_scheduled_intent().unwrap(),
            scheduler.pop_next_scheduled_intent().unwrap(),
        ];
        assert!(next_msgs.contains(&msg2));
        assert!(next_msgs.contains(&msg4));
        assert_eq!(scheduler.intents_blocked(), 2);

        // Complete msg2
        assert!(scheduler.complete(&msg2.inner).is_ok());
        // msg2 and msg4 should be available (they don't conflict)
        let next_intents = [
            scheduler.pop_next_scheduled_intent().unwrap(),
            scheduler.pop_next_scheduled_intent().unwrap(),
        ];
        assert!(next_intents.contains(&msg3));
        assert!(next_intents.contains(&msg5));
        assert_eq!(scheduler.intents_blocked(), 0);
    }
}

#[cfg(test)]
mod edge_cases_test {
    use magicblock_program::magic_scheduled_base_intent::MagicBaseIntent;

    use super::*;

    #[test]
    fn test_intent_without_pubkeys() {
        let mut scheduler = IntentScheduler::new();
        let mut msg = create_test_intent(1, &[]);
        msg.inner.base_intent = MagicBaseIntent::BaseActions(vec![]);

        // Should execute immediately since it has no pubkeys
        assert!(scheduler.schedule(msg.clone()).is_some());
        assert_eq!(scheduler.intents_blocked(), 0);
    }
}

#[cfg(test)]
mod complete_error_test {
    use magicblock_program::magic_scheduled_base_intent::CommittedAccount;
    use solana_account::Account;
    use solana_pubkey::pubkey;

    use super::*;

    #[test]
    fn test_complete_non_scheduled_message() {
        let mut scheduler = IntentScheduler::new();
        let msg = create_test_intent(
            1,
            &[pubkey!("1111111111111111111111111111111111111111111")],
        );

        // Attempt to complete message that was never scheduled
        let result = scheduler.complete(&msg.inner);
        assert!(matches!(
            result,
            Err(IntentSchedulerError::NonScheduledMessageError)
        ));
    }

    #[test]
    fn test_corrupted_intent_state_more_keys_initially() {
        let mut scheduler = IntentScheduler::new();
        let pubkey1 = pubkey!("1111111111111111111111111111111111111111111");
        let pubkey2 = pubkey!("21111111111111111111111111111111111111111111");

        // Schedule first intent
        let mut msg1 = create_test_intent(1, &[pubkey1, pubkey2]);
        assert!(scheduler.schedule(msg1.clone()).is_some());

        // Schedule second intent that conflicts with first
        let msg2 = create_test_intent(2, &[pubkey1]);
        assert!(scheduler.schedule(msg2.clone()).is_none());

        msg1.inner.get_committed_accounts_mut().unwrap().pop();

        // Attempt to complete msg1 - should detect corrupted state
        let result = scheduler.complete(&msg1.inner);
        assert!(matches!(
            result,
            Err(IntentSchedulerError::CorruptedIntentError)
        ));
    }

    #[test]
    fn test_corrupted_intent_state_less_keys_initially() {
        let mut scheduler = IntentScheduler::new();
        let pubkey1 = pubkey!("1111111111111111111111111111111111111111111");
        let pubkey2 = pubkey!("21111111111111111111111111111111111111111111");
        let pubkey3 = pubkey!("31111111111111111111111111111111111111111111");

        // Schedule first intent
        let mut msg1 = create_test_intent(1, &[pubkey1, pubkey2]);
        assert!(scheduler.schedule(msg1.clone()).is_some());

        msg1.inner
            .base_intent
            .get_committed_accounts_mut()
            .unwrap()
            .push(CommittedAccount {
                pubkey: pubkey3,
                account: Account::default(),
            });

        // Attempt to complete msg1 - should detect corrupted state
        let result = scheduler.complete(&msg1.inner);
        assert!(matches!(
            result,
            Err(IntentSchedulerError::CorruptedIntentError)
        ));
    }

    #[test]
    fn test_completing_blocked_message_complex() {
        let mut scheduler = IntentScheduler::new();
        let pubkey1 = pubkey!("1111111111111111111111111111111111111111111");
        let pubkey2 = pubkey!("21111111111111111111111111111111111111111111");

        // Schedule first intent for pubkey1 only
        let msg1 = create_test_intent(1, &[pubkey1]);
        assert!(scheduler.schedule(msg1.clone()).is_some());

        // Create second intent using both pubkeys
        let msg2 = create_test_intent(2, &[pubkey1, pubkey2]);
        // Manually add to blocked_keys without proper scheduling
        scheduler.schedule(msg2.clone());

        // Attempt to complete - should detect corrupted state
        let result = scheduler.complete(&msg2.inner);
        assert!(matches!(
            result,
            Err(IntentSchedulerError::CompletingBlockedIntentError)
        ));
    }

    #[test]
    fn test_completing_blocked_message() {
        let mut scheduler = IntentScheduler::new();
        let pubkey = pubkey!("1111111111111111111111111111111111111111111");

        // Schedule two intents for same pubkey
        let msg1 = create_test_intent(1, &[pubkey]);
        let msg2 = create_test_intent(2, &[pubkey]);

        // First executes immediately
        assert!(scheduler.schedule(msg1.clone()).is_some());
        // Second gets blocked
        assert!(scheduler.schedule(msg2.clone()).is_none());

        // Attempt to complete msg2 before msg1 - should detect corrupted state
        let result = scheduler.complete(&msg2.inner);
        assert!(matches!(
            result,
            Err(IntentSchedulerError::CompletingBlockedIntentError)
        ));
    }
}

// Helper function to create test intents
#[cfg(test)]
pub(crate) fn create_test_intent(
    id: u64,
    pubkeys: &[Pubkey],
) -> ScheduledBaseIntentWrapper {
    use magicblock_program::magic_scheduled_base_intent::{
        CommitType, CommittedAccount, MagicBaseIntent,
    };
    use solana_account::Account;
    use solana_sdk::{hash::Hash, transaction::Transaction};

    use crate::types::TriggerType;

    let mut intent = ScheduledBaseIntent {
        id,
        slot: 0,
        blockhash: Hash::default(),
        action_sent_transaction: Transaction::default(),
        payer: Pubkey::default(),
        base_intent: MagicBaseIntent::BaseActions(vec![]),
    };

    // Only set pubkeys if provided
    if !pubkeys.is_empty() {
        let committed_accounts = pubkeys
            .iter()
            .map(|&pubkey| CommittedAccount {
                pubkey,
                account: Account::default(),
            })
            .collect();

        intent.base_intent =
            MagicBaseIntent::Commit(CommitType::Standalone(committed_accounts));
    }

    ScheduledBaseIntentWrapper {
        inner: intent,
        trigger_type: TriggerType::OffChain,
    }
}
