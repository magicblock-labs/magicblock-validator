use std::collections::{hash_map::Entry, BTreeSet, HashMap, HashSet, VecDeque};

use magicblock_program::outbox_intent_bundles::OutboxIntentBundle;
use solana_pubkey::Pubkey;
use thiserror::Error;
use tracing::{error, warn};

pub(crate) const POISONED_INNER_MSG: &str =
    "Mutex on CommitSchedulerInner is poisoned.";

type IntentID = u64;
struct IntentMeta {
    num_keys: usize,
    intent: OutboxIntentBundle,
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
/// 3. `poisoned_keys`: Pubkeys touched by a failed or voided intent - once
///    poisoned, a pubkey rejects every future intent for the lifetime of
///    this scheduler (only a process restart clears it, since the outbox
///    will naturally retry whatever was poisoned)
///    - Key: Pubkey
///
/// # Scheduling Logic
///
/// 1. On intent arrival:
///     - Check if any required pubkey is poisoned: if so, poison the rest
///       of this intent's pubkeys too and reject it (it will never execute)
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
/// 4. On intent failure:
///     - Poison all of the failed intent's pubkeys, then walk every
///       successor reachable from them (transitively, via shared pubkeys)
///       and void it too - each voided intent poisons its own full pubkey
///       set the same way, so the cascade can't stop partway through a
///       dependency chain
///     - Intents that merely share a pubkey but aren't reachable (queued
///       *before* the point the cascade reaches, or on an unrelated key)
///       are untouched and keep executing normally
///     - See `poisoned_test` for the full algorithm writeup and worked
///       examples
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
    poisoned_keys: HashSet<Pubkey>,
}

impl IntentScheduler {
    pub fn new() -> Self {
        Self {
            blocked_keys: HashMap::new(),
            blocked_intents: HashMap::new(),
            poisoned_keys: HashSet::new(),
        }
    }

    /// Returns [`ScheduledIntentBundle`] if intent can be executed,
    /// otherwise consumes it and enqueues
    // TODO(edwin): tweak return type to reflect Poisoned, ScheduleResult
    pub fn schedule(
        &mut self,
        intent_bundle: OutboxIntentBundle,
    ) -> Option<OutboxIntentBundle> {
        let intent_id = intent_bundle.id;
        let pubkeys = intent_bundle.get_all_committed_pubkeys();
        if pubkeys.is_empty() {
            return Some(intent_bundle);
        };

        // Check that id is not duplicated
        if self.is_duplicate(intent_id) {
            // This is critical error as we shouldn't schedule duplicate Intents!
            // this requires investigation
            error!(
                intent_id,
                "CRITICAL! Attempt to schedule already scheduled intent"
            );
            return None;
        }

        // Check if intent is poisoned by existing poisonous keys
        let is_poisoned =
            pubkeys.iter().any(|el| self.poisoned_keys.contains(el));
        if is_poisoned {
            // Intent got poisoned by others
            warn!(
                intent_id,
                pubkeys = ?pubkeys,
                "Intent got poisoned"
            );
            self.poisoned_keys.extend(pubkeys);
            return None;
        }

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
                    intent: intent_bundle,
                },
            );
            None
        } else {
            Some(intent_bundle)
        }
    }

    /// Returns if same IntentId is scheduled
    /// To check duplicate scheduling its enough to check:
    /// 1. currently blocked
    /// 2. currently executing
    /// NOTE: under assumption that outer system doesn't schedule duplcates
    /// this can be ommitted to reduce execution time
    fn is_duplicate(&self, intent_id: IntentID) -> bool {
        if self.blocked_intents.contains_key(&intent_id) {
            true
        } else {
            let duplicate_executing =
                self.blocked_keys.iter().any(|(_, queue)| {
                    if let Some(executing_id) = queue.front() {
                        &intent_id == executing_id
                    } else {
                        false
                    }
                });

            duplicate_executing
        }
    }

    fn validate_executing(
        &self,
        intent_id: IntentID,
        pubkeys: &[Pubkey],
    ) -> IntentSchedulerResult<()> {
        if self.blocked_intents.contains_key(&intent_id) {
            return Err(IntentSchedulerError::CompletingBlockedIntentError);
        }

        // All front of queues contain current intent id
        let mut all_front = true;
        // Some of front queues contain intent id
        let mut some_front = false;
        for pubkey in pubkeys {
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
            Err(IntentSchedulerError::CorruptedIntentError)
        } else {
            Ok(())
        }
    }

    /// Completes Intent, cleaning up data after itself and allowing Intents to move forward
    /// NOTE: This doesn't unblock intent, hence Self::intents_blocked will return old value.
    /// NOTE: this shall be called on executing intents to finalize their execution.
    pub fn complete(
        &mut self,
        intent_bundle: &OutboxIntentBundle,
    ) -> IntentSchedulerResult<()> {
        // Release data for completed intent
        let intent_id = intent_bundle.id;
        let pubkeys = intent_bundle.get_all_committed_pubkeys();
        if pubkeys.is_empty() {
            // This means BaseAction, it doesn't have to be scheduled
            return Ok(());
        };

        // Validate that requested intent is executing indeed
        self.validate_executing(intent_id, &pubkeys)?;

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

    /// Processes failed intent. This leads to poison spreading over scheduled overlapping intents.
    /// Returns poisoned intents by failed intent.
    /// NOTE: this shall be called on executing intents to finalize their execution.
    /// NOTE: this shall be called only after multiple retries as it permanently poisons other intents as well
    pub fn failed(
        &mut self,
        intent_bundle: &OutboxIntentBundle,
    ) -> IntentSchedulerResult<Vec<OutboxIntentBundle>> {
        // Release data for completed intent
        let intent_id = intent_bundle.id;
        let pubkeys = intent_bundle.get_all_committed_pubkeys();
        if pubkeys.is_empty() {
            // This means Action only intent, it can't poisone anything
            return Ok(vec![]);
        };

        // Validate that requested intent is executing indeed
        self.validate_executing(intent_id, &pubkeys)?;

        // Poison intents
        let mut worklist = BTreeSet::new();
        worklist.insert(intent_id);

        for pubkey in pubkeys {
            self.poisoned_keys.insert(pubkey);
            let queue =
                self.blocked_keys.remove(&pubkey).expect("front-checked");
            worklist.extend(queue.into_iter().skip(1));
        }

        let mut poisoned = Vec::new();
        while let Some(intent_id) = worklist.pop_first() {
            let Some(meta) = self.blocked_intents.remove(&intent_id) else {
                continue;
            };

            let pubkeys = meta.intent.get_all_committed_pubkeys();
            for pubkey in &pubkeys {
                let Entry::Occupied(mut val) = self.blocked_keys.entry(*pubkey)
                else {
                    continue;
                };
                let Ok(pos) = val.get_mut().binary_search(&intent_id) else {
                    // Queue was already drained
                    continue;
                };

                // Remove items in queue starting with intent_id. All following intens are poisoned
                let poisoned_iter = val.get_mut().drain(pos..).skip(1);
                worklist.extend(poisoned_iter);
                if val.get().is_empty() {
                    val.remove();
                }
            }

            self.poisoned_keys.extend(pubkeys);
            poisoned.push(meta.intent);
        }

        Ok(poisoned)
    }

    // Returns [`ScheduledBaseIntent`] that can be executed
    pub fn pop_next_scheduled_intent(&mut self) -> Option<OutboxIntentBundle> {
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
    #[error("Intent touched poisoned pubkeys")]
    IntentPoisonedError(Vec<Pubkey>),
}

pub type IntentSchedulerResult<T, E = IntentSchedulerError> = Result<T, E>;

/// Set of simple tests
#[cfg(test)]
mod simple_test {
    use solana_pubkey::pubkey;

    use super::*;
    use crate::test_utils;

    fn setup() {
        test_utils::init_test_logger();
    }

    #[test]
    fn test_empty_scheduler() {
        setup();
        let mut scheduler = IntentScheduler::new();
        assert_eq!(scheduler.intents_blocked(), 0);
        assert!(scheduler.pop_next_scheduled_intent().is_none());
    }

    /// Ensure intents with non-conflicting set of keys can run in parallel
    #[test]
    fn test_non_conflicting_intents() {
        setup();
        let mut scheduler = IntentScheduler::new();
        let msg1 = create_test_intent(
            1,
            &[pubkey!("1111111111111111111111111111111111111111111")],
            false,
        );
        let msg2 = create_test_intent(
            2,
            &[pubkey!("22222222222222222222222222222222222222222222")],
            false,
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
        setup();
        const NUM_INTENTS: u64 = 10;

        let mut scheduler = IntentScheduler::new();
        let pubkey = pubkey!("1111111111111111111111111111111111111111111");
        let msg1 = create_test_intent(1, &[pubkey], false);

        // First message executes immediately
        assert!(scheduler.schedule(msg1).is_some());
        for id in 2..=NUM_INTENTS {
            let msg = create_test_intent(id, &[pubkey], false);
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
    use crate::test_utils;

    fn setup() {
        test_utils::init_test_logger();
    }

    #[test]
    fn test_completion_unblocks_intents() {
        setup();
        let mut scheduler = IntentScheduler::new();
        let pubkey = pubkey!("1111111111111111111111111111111111111111111");
        let msg1 = create_test_intent(1, &[pubkey], false);
        let msg2 = create_test_intent(2, &[pubkey], false);

        // First intent executes immediately
        let executed = scheduler.schedule(msg1.clone()).unwrap();
        // Second intent gets blocked
        assert!(scheduler.schedule(msg2.clone()).is_none());
        assert_eq!(scheduler.intents_blocked(), 1);

        // Complete first intent
        assert!(scheduler.complete(&executed).is_ok());

        let next = scheduler.pop_next_scheduled_intent().unwrap();
        assert_eq!(next, msg2);
        assert_eq!(scheduler.intents_blocked(), 0);
    }

    #[test]
    fn test_multiple_blocked_intents() {
        setup();
        let mut scheduler = IntentScheduler::new();
        let pubkey = pubkey!("1111111111111111111111111111111111111111111");
        let msg1 = create_test_intent(1, &[pubkey], false);
        let msg2 = create_test_intent(2, &[pubkey], false);
        let msg3 = create_test_intent(3, &[pubkey], false);

        // First intent executes immediately
        let executed = scheduler.schedule(msg1.clone()).unwrap();
        // Others get blocked
        assert!(scheduler.schedule(msg2.clone()).is_none());
        assert!(scheduler.schedule(msg3.clone()).is_none());
        assert_eq!(scheduler.intents_blocked(), 2);

        // Complete first intent
        assert!(scheduler.complete(&executed).is_ok());

        // Second intent should now be available
        let expected_msg2 = scheduler.pop_next_scheduled_intent().unwrap();
        assert_eq!(expected_msg2, msg2);
        assert_eq!(scheduler.intents_blocked(), 1);

        // Complete second intent
        assert!(scheduler.complete(&expected_msg2).is_ok());

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
    use crate::test_utils;

    fn setup() {
        test_utils::init_test_logger();
    }

    /// Case:
    /// executing: `[a1, a2, a3] [b1, b2, b3]` - 1
    /// blocked:   `[a1,         b1]` - 2
    /// arriving:  `[a1,     a3]` - 3
    #[test]
    fn test_edge_case_1_earlier_intent_blocks_later_overlapping() {
        setup();
        let mut scheduler = IntentScheduler::new();
        let a1 = pubkey!("1111111111111111111111111111111111111111111");
        let a2 = pubkey!("21111111111111111111111111111111111111111111");
        let a3 = pubkey!("31111111111111111111111111111111111111111111");
        let b1 = pubkey!("41111111111111111111111111111111111111111111");
        let b2 = pubkey!("51111111111111111111111111111111111111111111");
        let b3 = pubkey!("61111111111111111111111111111111111111111111");

        // intent 1: [a1, a2, a3]
        let msg1_keys = vec![a1, a2, a3];
        let msg1 = create_test_intent(1, &msg1_keys, false);
        assert!(scheduler.schedule(msg1.clone()).is_some());
        assert_eq!(scheduler.intents_blocked(), 0);

        // intent 2:  [b1, b2, b3]
        let msg2_keys = vec![b1, b2, b3];
        let msg2 = create_test_intent(2, &msg2_keys, false);
        assert!(scheduler.schedule(msg2.clone()).is_some());
        assert_eq!(scheduler.intents_blocked(), 0);

        // intent 3: [a1, b1] - blocked by msg1 & msg2
        let msg3_keys = vec![a1, b1];
        let msg3 = create_test_intent(3, &msg3_keys, false);
        assert!(scheduler.schedule(msg3.clone()).is_none());
        assert_eq!(scheduler.intents_blocked(), 1);

        // intent 4: [a1, a3] - blocked by msg1 & msg3
        let msg4_keys = vec![a1, a3];
        let msg4 = create_test_intent(4, &msg4_keys, false);
        assert!(scheduler.schedule(msg4.clone()).is_none());
        assert_eq!(scheduler.intents_blocked(), 2);

        // Complete msg1
        assert!(scheduler.complete(&msg1).is_ok());
        // None of the intents can execute yet
        // msg3 is blocked msg2
        // msg4 is blocked by msg3
        assert!(scheduler.pop_next_scheduled_intent().is_none());

        // Complete msg2
        assert!(scheduler.complete(&msg2).is_ok());
        // Now msg3 is unblocked
        let next = scheduler.pop_next_scheduled_intent().unwrap();
        assert_eq!(next, msg3);
        assert_eq!(scheduler.intents_blocked(), 1);
        // Complete msg3
        assert!(scheduler.complete(&next).is_ok());

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
        setup();
        let mut scheduler = IntentScheduler::new();
        let a1 = pubkey!("1111111111111111111111111111111111111111111");
        let a2 = pubkey!("21111111111111111111111111111111111111111111");
        let a3 = pubkey!("31111111111111111111111111111111111111111111");
        let c1 = pubkey!("41111111111111111111111111111111111111111111");
        let c2 = pubkey!("51111111111111111111111111111111111111111111");

        // intent 1: [a1, a2, a3] (executing)
        let msg1_keys = vec![a1, a2, a3];
        let msg1 = create_test_intent(1, &msg1_keys, false);

        // intent 2: [c1, a1] (blocked by msg1)
        let msg2_keys = vec![c1, a1];
        let msg2 = create_test_intent(2, &msg2_keys, false);

        // intent 3: [c2, c1] (arriving later)
        let msg3_keys = vec![c2, c1];
        let msg3 = create_test_intent(3, &msg3_keys, false);

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
        assert!(scheduler.complete(&executed_msg1).is_ok());

        // Now only msg2 should be available (not msg3)
        let expected_msg2 = scheduler.pop_next_scheduled_intent().unwrap();
        assert_eq!(expected_msg2, msg2);
        assert_eq!(scheduler.intents_blocked(), 1);
        // msg 3 still should be blocked
        assert_eq!(scheduler.pop_next_scheduled_intent(), None);

        // Complete msg2
        assert!(scheduler.complete(&expected_msg2).is_ok());

        // Now msg3 should be available
        let expected_msg3 = scheduler.pop_next_scheduled_intent().unwrap();
        assert_eq!(expected_msg3, msg3);
        assert_eq!(scheduler.intents_blocked(), 0);
    }

    #[test]
    fn test_complex_contention_scenario() {
        setup();
        let mut scheduler = IntentScheduler::new();
        let a = pubkey!("1111111111111111111111111111111111111111111");
        let b = pubkey!("21111111111111111111111111111111111111111111");
        let c = pubkey!("31111111111111111111111111111111111111111111");

        // intents with various key combinations
        let msg1 = create_test_intent(1, &[a, b], false);
        let msg2 = create_test_intent(2, &[a, c], false);
        let msg3 = create_test_intent(3, &[c], false);
        let msg4 = create_test_intent(4, &[b], false);
        let msg5 = create_test_intent(5, &[a], false);

        // msg1 executes immediately
        let executed1 = scheduler.schedule(msg1.clone()).unwrap();
        // Others get blocked
        assert!(scheduler.schedule(msg2.clone()).is_none());
        assert!(scheduler.schedule(msg3.clone()).is_none());
        assert!(scheduler.schedule(msg4.clone()).is_none());
        assert!(scheduler.schedule(msg5.clone()).is_none());
        assert_eq!(scheduler.intents_blocked(), 4);

        // Complete msg1
        assert!(scheduler.complete(&executed1).is_ok());

        // msg2 and msg4 should be available (they don't conflict)
        let next_msgs = [
            scheduler.pop_next_scheduled_intent().unwrap(),
            scheduler.pop_next_scheduled_intent().unwrap(),
        ];
        assert!(next_msgs.contains(&msg2));
        assert!(next_msgs.contains(&msg4));
        assert_eq!(scheduler.intents_blocked(), 2);

        // Complete msg2
        assert!(scheduler.complete(&msg2).is_ok());
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
    use magicblock_core::intent::MagicIntentBundle;

    use super::*;
    use crate::test_utils;

    fn setup() {
        test_utils::init_test_logger();
    }

    #[test]
    fn test_intent_without_pubkeys() {
        setup();
        let mut scheduler = IntentScheduler::new();
        let mut msg = create_test_intent(1, &[], false);
        msg.inner.intent_bundle = MagicIntentBundle::default();

        // Should execute immediately since it has no pubkeys
        assert!(scheduler.schedule(msg.clone()).is_some());
        assert_eq!(scheduler.intents_blocked(), 0);
    }
}

#[cfg(test)]
mod complete_error_test {
    use magicblock_core::intent::types::CommittedAccount;
    use solana_account::Account;
    use solana_pubkey::pubkey;

    use super::*;
    use crate::test_utils;

    fn setup() {
        test_utils::init_test_logger();
    }

    #[test]
    fn test_complete_non_scheduled_message() {
        setup();
        let mut scheduler = IntentScheduler::new();
        let msg = create_test_intent(
            1,
            &[pubkey!("1111111111111111111111111111111111111111111")],
            false,
        );

        // Attempt to complete message that was never scheduled
        let result = scheduler.complete(&msg);
        assert!(matches!(
            result,
            Err(IntentSchedulerError::NonScheduledMessageError)
        ));
    }

    #[test]
    fn test_corrupted_intent_state_more_keys_initially() {
        setup();
        let mut scheduler = IntentScheduler::new();
        let pubkey1 = pubkey!("1111111111111111111111111111111111111111111");
        let pubkey2 = pubkey!("21111111111111111111111111111111111111111111");

        // Schedule first intent
        let mut msg1 = create_test_intent(1, &[pubkey1, pubkey2], false);
        assert!(scheduler.schedule(msg1.clone()).is_some());

        // Schedule second intent that conflicts with first
        let msg2 = create_test_intent(2, &[pubkey1], false);
        assert!(scheduler.schedule(msg2.clone()).is_none());

        msg1.inner.get_commit_intent_accounts_mut().unwrap().pop();

        // Attempt to complete msg1 - should detect corrupted state
        let result = scheduler.complete(&msg1);
        assert!(matches!(
            result,
            Err(IntentSchedulerError::CorruptedIntentError)
        ));
    }

    #[test]
    fn test_corrupted_intent_state_less_keys_initially() {
        setup();
        let mut scheduler = IntentScheduler::new();
        let pubkey1 = pubkey!("1111111111111111111111111111111111111111111");
        let pubkey2 = pubkey!("21111111111111111111111111111111111111111111");
        let pubkey3 = pubkey!("31111111111111111111111111111111111111111111");

        // Schedule first intent
        let mut msg1 = create_test_intent(1, &[pubkey1, pubkey2], false);
        assert!(scheduler.schedule(msg1.clone()).is_some());

        msg1.inner
            .intent_bundle
            .get_commit_intent_accounts_mut()
            .unwrap()
            .push(CommittedAccount {
                pubkey: pubkey3,
                account: Account::default(),
                remote_slot: Default::default(),
            });

        // Attempt to complete msg1 - should detect corrupted state
        let result = scheduler.complete(&msg1);
        assert!(matches!(
            result,
            Err(IntentSchedulerError::CorruptedIntentError)
        ));
    }

    #[test]
    fn test_completing_blocked_message_complex() {
        setup();
        let mut scheduler = IntentScheduler::new();
        let pubkey1 = pubkey!("1111111111111111111111111111111111111111111");
        let pubkey2 = pubkey!("21111111111111111111111111111111111111111111");

        // Schedule first intent for pubkey1 only
        let msg1 = create_test_intent(1, &[pubkey1], false);
        assert!(scheduler.schedule(msg1.clone()).is_some());

        // Create second intent using both pubkeys
        let msg2 = create_test_intent(2, &[pubkey1, pubkey2], false);
        // Manually add to blocked_keys without proper scheduling
        scheduler.schedule(msg2.clone());

        // Attempt to complete - should detect corrupted state
        let result = scheduler.complete(&msg2);
        assert!(matches!(
            result,
            Err(IntentSchedulerError::CompletingBlockedIntentError)
        ));
    }

    #[test]
    fn test_completing_blocked_message() {
        setup();
        let mut scheduler = IntentScheduler::new();
        let pubkey = pubkey!("1111111111111111111111111111111111111111111");

        // Schedule two intents for same pubkey
        let msg1 = create_test_intent(1, &[pubkey], false);
        let msg2 = create_test_intent(2, &[pubkey], false);

        // First executes immediately
        assert!(scheduler.schedule(msg1.clone()).is_some());
        // Second gets blocked
        assert!(scheduler.schedule(msg2.clone()).is_none());

        // Attempt to complete msg2 before msg1 - should detect corrupted state
        let result = scheduler.complete(&msg2);
        assert!(matches!(
            result,
            Err(IntentSchedulerError::CompletingBlockedIntentError)
        ));
    }
}

#[cfg(test)]
mod intent_bundle_test {
    use solana_pubkey::pubkey;

    use super::*;

    /// Bundle contains BOTH Commit and CommitAndUndelegate.
    /// Scheduler must treat committed pubkeys as UNION across both.
    #[test]
    fn test_bundle_with_commit_and_cau_blocks_on_union() {
        let mut scheduler = IntentScheduler::new();

        let a = pubkey!("1111111111111111111111111111111111111111111");
        let b = pubkey!("21111111111111111111111111111111111111111111");
        let c = pubkey!("31111111111111111111111111111111111111111111");

        // msg1 has commit[a] and cau[b]
        let msg1 = create_test_intent_bundle(1, &[a], &[b]);

        // msg2 conflicts with commit key (a)
        let msg2 = create_test_intent(2, &[a], false);
        // msg3 conflicts with cau key (b)
        let msg3 = create_test_intent(3, &[b], false);
        // msg4 is unrelated (c), should run immediately even while msg1 executes
        let msg4 = create_test_intent(4, &[c], false);

        // msg1 executes immediately
        let executed1 = scheduler.schedule(msg1.clone()).unwrap();
        assert_eq!(executed1, msg1);
        assert_eq!(scheduler.intents_blocked(), 0);

        // msg2 and msg3 should be blocked due to union keys [a, b]
        assert!(scheduler.schedule(msg2.clone()).is_none());
        assert!(scheduler.schedule(msg3.clone()).is_none());
        assert_eq!(scheduler.intents_blocked(), 2);

        // msg4 doesn't conflict, should execute immediately
        assert!(scheduler.schedule(msg4.clone()).is_some());
        assert_eq!(scheduler.intents_blocked(), 2);
    }

    /// After completing a bundle with both intents, the blocked intents should become eligible.
    #[test]
    fn test_bundle_with_commit_and_cau_unblocks_correctly() {
        let mut scheduler = IntentScheduler::new();

        let a = pubkey!("1111111111111111111111111111111111111111111");
        let b = pubkey!("21111111111111111111111111111111111111111111");

        // msg1 has commit[a] and cau[b]
        let msg1 = create_test_intent_bundle(1, &[a], &[b]);
        // both should be blocked behind msg1
        let msg2 = create_test_intent(2, &[a], false);
        let msg3 = create_test_intent(3, &[b], false);

        // msg1 executes immediately
        let executed1 = scheduler.schedule(msg1.clone()).unwrap();
        // enqueue blockers
        assert!(scheduler.schedule(msg2.clone()).is_none());
        assert!(scheduler.schedule(msg3.clone()).is_none());
        assert_eq!(scheduler.intents_blocked(), 2);

        // Complete msg1
        assert!(scheduler.complete(&executed1).is_ok());

        // Now both msg2 and msg3 are eligible (order doesn't matter)
        let next1 = scheduler.pop_next_scheduled_intent().unwrap();
        assert!(next1 == msg2 || next1 == msg3);
        assert_eq!(scheduler.intents_blocked(), 1);

        assert!(scheduler.complete(&next1).is_ok());

        let next2 = scheduler.pop_next_scheduled_intent().unwrap();
        assert!(next2 == msg2 || next2 == msg3);
        assert_ne!(next1, next2);
        assert_eq!(scheduler.intents_blocked(), 0);
    }
}

#[cfg(test)]
mod poisoned_test {
    use solana_pubkey::pubkey;

    use super::*;
    use crate::test_utils;

    fn setup() {
        test_utils::init_test_logger();
    }

    /// # Case 1 — poison propagates through a chain, but spares an
    /// unrelated *executing* intent
    ///
    /// ```text
    /// F = [a1, a2]        executing, front of a1 and a2
    /// G = [b1, b2]        executing, front of b1 and b2   <- this one fails
    /// X = [a2, b1]        queued behind F on a2, behind G on b1
    /// Y = [a1, a2]        queued behind F on a1, behind X on a2
    ///
    ///              a1   a2   b1   b2
    ///        pos0:  F    F    G    G
    ///        pos1:  Y    X    X    .
    ///        pos2:  .    Y    .    .
    /// ```
    ///
    /// `G` fails. Poisoning `b1` reaches `X` (shares `b1`), which in turn
    /// reaches `Y` (shares `a2`) — both get voided. `F` must be left
    /// completely alone: it shares pubkeys with the voided chain, but not
    /// a dependency edge — it's positioned *before* `X` on `a2`, not
    /// after, so the drain-from-position walk never reaches it.
    #[test]
    fn test_case_1_poison_chain_spares_the_other_executing_intent() {
        setup();
        let mut scheduler = IntentScheduler::new();
        let a1 = pubkey!("1111111111111111111111111111111111111111111");
        let a2 = pubkey!("21111111111111111111111111111111111111111111");
        let b1 = pubkey!("31111111111111111111111111111111111111111111");
        let b2 = pubkey!("41111111111111111111111111111111111111111111");

        let executing_a = create_test_intent(1, &[a1, a2], false);
        let executing_b = create_test_intent(2, &[b1, b2], false);
        assert!(scheduler.schedule(executing_a.clone()).is_some());
        assert!(scheduler.schedule(executing_b.clone()).is_some());

        let x = create_test_intent(3, &[a2, b1], false);
        let y = create_test_intent(4, &[a1, a2], false);
        assert!(scheduler.schedule(x.clone()).is_none());
        assert!(scheduler.schedule(y.clone()).is_none());

        let poisoned = scheduler.failed(&executing_b).unwrap();
        let mut ids: Vec<_> = poisoned.iter().map(|i| i.id).collect();
        ids.sort();
        assert_eq!(
            ids,
            vec![x.id, y.id],
            "both blocked intents on the chain must be voided"
        );

        // The unrelated executing intent is untouched: it can still complete().
        assert!(scheduler.complete(&executing_a).is_ok());
    }

    /// # Case 2 — reachable intents are ID-ordered
    ///
    /// ```text
    /// F  = [a1, a2]        executing, front of a1 and a2
    /// I1 = [b1, b2]        executing, front of b1 and b2
    /// X  = [a2, b1]        queued behind F on a2, behind I1 on b1
    /// I2 = [a1, a2]        queued behind F on a1, behind X on a2
    ///
    ///               a1   a2   b1   b2
    ///         pos0:  F    F   I1   I1
    ///         pos1: I2    X    X    .
    ///         pos2:  .   I2    .    .
    /// ```
    ///
    /// `I1` reaches `X` (shares `b1`), which reaches `I2` (shares `a2`) —
    /// so `I1` and `I2` are *not* isolated from each other. Reachable
    /// intents must be ID-ordered: `I1.id < I2.id`.
    ///
    /// Handwaving proof: suppose instead `I2.id < I1.id`. Then `I2` would
    /// have been admitted first and would occupy `a2`'s queue ahead of
    /// whatever comes to share it with `I1` — but `I1` is what's supposed
    /// to reach `I2`, not the other way around. Contradiction.
    #[test]
    fn test_case_2_reachable_intents_are_id_ordered() {
        setup();
        let mut scheduler = IntentScheduler::new();
        let a1 = pubkey!("1111111111111111111111111111111111111111111");
        let a2 = pubkey!("21111111111111111111111111111111111111111111");
        let b1 = pubkey!("31111111111111111111111111111111111111111111");
        let b2 = pubkey!("41111111111111111111111111111111111111111111");

        let executing_a = create_test_intent(1, &[a1, a2], false);
        let i1 = create_test_intent(2, &[b1, b2], false);
        assert!(scheduler.schedule(executing_a).is_some());
        assert!(scheduler.schedule(i1.clone()).is_some());

        let bridge = create_test_intent(3, &[a2, b1], false);
        let i2 = create_test_intent(4, &[a1, a2], false);
        assert!(scheduler.schedule(bridge).is_none());
        assert!(scheduler.schedule(i2.clone()).is_none());

        assert!(i1.id < i2.id, "reachable intents must be ID-ordered");

        // Empirically: I2 is reachable from I1 (not isolated) - failing I1
        // must void I2 too.
        let poisoned = scheduler.failed(&i1).unwrap();
        assert!(poisoned.iter().any(|p| p.id == i2.id));
    }

    /// # Case 3 — which intents to exclude
    ///
    /// ```text
    /// F  = [a1, a2]        executing, front of a1 and a2
    /// I1 = [b1, b2]        executing, front of b1 and b2   <- fails
    /// I2 = [a1, a2]        queued behind F on a1 and a2
    /// I3 = [a2, b1]        queued behind I2 on a2, behind I1 on b1
    /// I4 = [a1, a2]        queued behind I2 on a1, behind I3 on a2
    ///
    ///               a1   a2   b1   b2
    ///         pos0:  F    F   I1   I1
    ///         pos1: I2   I2   I3    .
    ///         pos2: I4   I3    .    .
    ///         pos3:  .   I4    .    .
    /// ```
    ///
    /// `I1` fails: `[a1, a2, b1, b2]` all become poisoned (every pubkey
    /// touched by a voided intent, unconditionally). `I3` and `I4` are
    /// voided. `I2` is *not*: it was queued for `a2` before `I3` was, so
    /// it's isolated from the chain — no `id`-based heuristic needed, the
    /// drain-from-position walk simply never reaches it — and it survives
    /// to execute normally.
    #[test]
    fn test_case_3_isolated_intent_survives_the_cascade() {
        setup();
        let mut scheduler = IntentScheduler::new();
        let a1 = pubkey!("1111111111111111111111111111111111111111111");
        let a2 = pubkey!("21111111111111111111111111111111111111111111");
        let b1 = pubkey!("31111111111111111111111111111111111111111111");
        let b2 = pubkey!("41111111111111111111111111111111111111111111");

        let executing_a = create_test_intent(1, &[a1, a2], false);
        let i1 = create_test_intent(2, &[b1, b2], false);
        assert!(scheduler.schedule(executing_a.clone()).is_some());
        assert!(scheduler.schedule(i1.clone()).is_some());

        let i2 = create_test_intent(3, &[a1, a2], false);
        let i3 = create_test_intent(4, &[a2, b1], false);
        let i4 = create_test_intent(5, &[a1, a2], false);
        assert!(scheduler.schedule(i2.clone()).is_none());
        assert!(scheduler.schedule(i3.clone()).is_none());
        assert!(scheduler.schedule(i4.clone()).is_none());

        let poisoned = scheduler.failed(&i1).unwrap();
        let mut ids: Vec<_> = poisoned.iter().map(|p| p.id).collect();
        ids.sort();
        assert_eq!(ids, vec![i3.id, i4.id], "I2 must not be voided");

        // Every pubkey touched by a voided intent rejects new scheduling,
        // even a1/a2 where I2 is still healthily queued.
        for pk in [a1, a2, b1, b2] {
            let probe = create_test_intent(100, &[pk], false);
            assert!(
                scheduler.schedule(probe).is_none(),
                "{pk} must reject new scheduling after the cascade"
            );
        }

        // I2 survives and executes normally once executing_a completes.
        assert!(scheduler.complete(&executing_a).is_ok());
        let next = scheduler.pop_next_scheduled_intent().unwrap();
        assert_eq!(next.id, i2.id);
    }

    /// # Case 4 — a chain of single-key overlaps cascades end to end
    ///
    /// Unlike Case 1/3, there's no parallel branch here to survive: each
    /// intent overlaps the next through exactly one shared pubkey, so the
    /// cascade has nowhere to stop until it consumes the whole chain.
    ///
    /// ```text
    /// I1 = [b2]            executing, front of b2 (single key)
    /// I2 = [b1, b2]        front of b1, queued behind I1 on b2
    /// I3 = [a2, b1]        front of a2, queued behind I2 on b1
    /// I4 = [a1, a2]        front of a1, queued behind I3 on a2
    ///
    ///               a1   a2   b1   b2
    ///         pos0: I4   I3   I2   I1
    ///         pos1:  .   I4   I3   I2
    ///         pos2:  .    .    .    .
    ///         pos3:  .    .    .    .
    /// ```
    ///
    /// `I1` fails. `I2` is reachable via `b2`, `I3` via `b1`, `I4` via
    /// `a2` — every link in the chain gets voided, and every pubkey the
    /// chain ever touched (`a1, a2, b1, b2`) is poisoned.
    #[test]
    fn test_case_4_full_chain_cascades_through_single_key_overlaps() {
        setup();
        let mut scheduler = IntentScheduler::new();
        let a1 = pubkey!("1111111111111111111111111111111111111111111");
        let a2 = pubkey!("21111111111111111111111111111111111111111111");
        let b1 = pubkey!("31111111111111111111111111111111111111111111");
        let b2 = pubkey!("41111111111111111111111111111111111111111111");

        let i1 = create_test_intent(1, &[b2], false);
        let i2 = create_test_intent(2, &[b1, b2], false);
        let i3 = create_test_intent(3, &[a2, b1], false);
        let i4 = create_test_intent(4, &[a1, a2], false);
        assert!(scheduler.schedule(i1.clone()).is_some());
        assert!(scheduler.schedule(i2.clone()).is_none());
        assert!(scheduler.schedule(i3.clone()).is_none());
        assert!(scheduler.schedule(i4.clone()).is_none());

        let poisoned = scheduler.failed(&i1).unwrap();
        let mut ids: Vec<_> = poisoned.iter().map(|p| p.id).collect();
        ids.sort();
        assert_eq!(
            ids,
            vec![i2.id, i3.id, i4.id],
            "the whole chain must be voided, nothing left to survive"
        );

        for pk in [a1, a2, b1, b2] {
            let probe = create_test_intent(100, &[pk], false);
            assert!(
                scheduler.schedule(probe).is_none(),
                "{pk} must reject new scheduling after the cascade"
            );
        }
    }

    /// # Case 5 — a diamond: two chains fan out and re-converge on one intent
    ///
    /// `X` is reachable from the failed intent through *two* independent
    /// paths. The worklist has to discover it twice and void it once.
    ///
    /// ```text
    /// F = [p1, p2]         executing, front of p1 and p2   <- this one fails
    /// A = [p1, q1]         queued behind F on p1, front of q1
    /// B = [p2, q2]         queued behind F on p2, front of q2
    /// X = [q1, q2]         queued behind A on q1, behind B on q2
    ///
    ///               p1   p2   q1   q2
    ///         pos0:  F    F    A    B
    ///         pos1:  A    B    X    X
    ///
    ///         F
    ///        / \
    ///       A   B
    ///        \ /
    ///         X
    /// ```
    ///
    /// `F` fails. `A` and `B` are each poisoned directly; walking `A`
    /// reaches `X` via `q1`, and walking `B` reaches `X` via `q2` — two
    /// discovery paths landing on the same intent. The worklist is a
    /// `BTreeSet<IntentID>`, so `X`'s id is only ever present once: it's
    /// popped and voided exactly once, and by the time it's processed both
    /// `q1` and `q2` are already drained, so both lookups on `X`'s own
    /// pubkeys hit the harmless `Entry::Vacant` bail-out.
    #[test]
    fn test_case_5_diamond_merge_is_voided_exactly_once() {
        setup();
        let mut scheduler = IntentScheduler::new();
        let p1 = pubkey!("1111111111111111111111111111111111111111111");
        let p2 = pubkey!("21111111111111111111111111111111111111111111");
        let q1 = pubkey!("31111111111111111111111111111111111111111111");
        let q2 = pubkey!("41111111111111111111111111111111111111111111");

        let f = create_test_intent(1, &[p1, p2], false);
        let a = create_test_intent(2, &[p1, q1], false);
        let b = create_test_intent(3, &[p2, q2], false);
        let x = create_test_intent(4, &[q1, q2], false);
        assert!(scheduler.schedule(f.clone()).is_some());
        assert!(scheduler.schedule(a.clone()).is_none());
        assert!(scheduler.schedule(b.clone()).is_none());
        assert!(scheduler.schedule(x.clone()).is_none());

        let poisoned = scheduler.failed(&f).unwrap();
        let mut ids: Vec<_> = poisoned.iter().map(|p| p.id).collect();
        ids.sort();
        assert_eq!(
            ids,
            vec![a.id, b.id, x.id],
            "X must be voided exactly once despite two discovery paths"
        );

        for pk in [p1, p2, q1, q2] {
            let probe = create_test_intent(100, &[pk], false);
            assert!(
                scheduler.schedule(probe).is_none(),
                "{pk} must reject new scheduling after the cascade"
            );
        }
    }

    /// `schedule()`'s admission-time poison check extends `poisoned_keys`
    /// with a rejected intent's *entire* pubkey set, same as `failed()`
    /// does for a voided intent. This is intentional, not a special case:
    /// a rejected intent is an atomic bundle that will never execute, so
    /// every pubkey it touches is equally hypothetical, whether or not the
    /// scheduler happened to have already materialized it into
    /// `blocked_keys` before the rejection was detected.
    #[test]
    fn test_rejection_poisons_the_rejected_intents_full_pubkey_set() {
        setup();
        let mut scheduler = IntentScheduler::new();
        let a1 = pubkey!("1111111111111111111111111111111111111111111");
        let c1 = pubkey!("21111111111111111111111111111111111111111111");

        // F touches only a1, executes immediately (nothing queued behind
        // it), then fails - poisons exactly {a1}.
        let f = create_test_intent(1, &[a1], false);
        let executed_f = scheduler.schedule(f).unwrap();
        let voided = scheduler.failed(&executed_f).unwrap();
        assert!(voided.is_empty());

        // Z touches the poisoned a1 *and* c1 in one atomic bundle. Since
        // Z can never execute, its effect on c1 never happens either -
        // rejecting Z must poison c1 too, not just a1.
        let z = create_test_intent(2, &[a1, c1], false);
        assert!(scheduler.schedule(z).is_none());

        // W touches only c1. A caller could construct W assuming Z's
        // (never-landed) effect on c1 already happened - e.g. undelegating
        // c1 assuming funds Z was supposed to deposit. W must be rejected.
        let w = create_test_intent(3, &[c1], false);
        assert!(
            scheduler.schedule(w).is_none(),
            "c1 was touched by Z, which will never execute; schedule() \
             must poison it so a future intent can't assume Z's effects \
             already landed"
        );
    }

    /// Case:
    /// blocked_keys represented as matrix
    /// topmost are about to execute
    /// Numbers represent intent id
    ///  a1, a2, b1, b2
    /// [[1, 1, 0, 0]
    ///  [2, 2, 1, 3]
    ///
    ///Failed 0, fails 1, where poison spreads on a1, a2
    /// flushin b1 we populate worklist with id1
    ///we find a1,a2 and also populate worklist with 2s
    /// also 3 will be added
    fn test_poison_spreading() {}

    /// # Poisoning: how a failed intent's dependents are found and voided
    ///
    /// ## Why
    ///
    /// When an executing intent fails, everything queued behind it may be
    /// relying on effects that never happened. If we leave those successors
    /// sitting in `blocked_intents` forever, [`super::IntentScheduler::pop_next_scheduled_intent`]
    /// can never make progress on the pubkeys they hold, `intents_blocked()`
    /// never shrinks, and (upstream, in the engine) the scheduler eventually
    /// panics once capacity is exhausted and no executor remains to drain it.
    /// [`super::IntentScheduler::failed`] exists to unstick this: it finds
    /// every intent that can no longer safely run, removes it from the
    /// scheduler, and reports it so a fresh attempt can be made later
    /// (nothing is discarded — the intent's outbox record is untouched and
    /// gets picked up again by the normal recovery scan, e.g. on restart).
    ///
    /// ## Definitions
    ///
    /// - **Dependency.** Intent `B` depends on intent `A` if `B` was queued
    ///   behind `A` because they share a pubkey. The FIFO blocking scheme
    ///   exists precisely because `B` may assume `A`'s effects on that
    ///   pubkey already landed — e.g. `A` transfers funds *into* an account,
    ///   `B` spends *from* it.
    /// - **Reachable.** `B` is reachable from `A` if there is a chain of
    ///   dependencies `A -> X1 -> X2 -> ... -> B`, where each arrow is one
    ///   "queued directly behind, on a shared pubkey" edge. Sharing a pubkey
    ///   alone is not enough — the edge only exists in the successor
    ///   direction (see the worked example below, where `I1` shares `a2`
    ///   with the chain but is *not* reachable from it).
    /// - **Isolated.** Two intents are isolated from each other if neither
    ///   is reachable from the other. Isolated intents may still share a
    ///   pubkey; they just never depend on one another through it.
    /// - **Voided intent.** An intent reachable from a failed intent. It
    ///   will never execute, is removed from `blocked_intents`, and is
    ///   returned by `failed()`.
    /// - **Poisoned key.** A pubkey no longer eligible for *new* scheduling
    ///   (`schedule()` rejects any incoming intent that touches it). A key
    ///   is poisoned either because an intent that actually executed
    ///   failed on it, or because a voided intent touched it — see
    ///   "Why poisoning is unconditional" below for why the latter applies
    ///   even when the key still has isolated, healthy intents on it.
    ///   `poisoned_keys` lives only in this `IntentScheduler` instance and
    ///   is cleared by process restart, not by anything within `failed()`
    ///   or `complete()`.
    ///
    /// ## Lemma: reachable intents are ID-ordered
    ///
    /// *If `B` is reachable from `A`, then `A.id < B.id`.*
    ///
    /// **Proof.** It's enough to show this for one dependency edge
    /// (`A -> B` directly); the general case follows by chaining edges,
    /// since `<` is transitive. `schedule()` only ever appends to the back
    /// of a pubkey's queue, in call order, so a queue's contents are always
    /// sorted ascending by id. `B` depends on `A` means `B` was scheduled
    /// while `A` already occupied their shared pubkey's queue — i.e. `B`
    /// arrived, and therefore was assigned its id, *after* `A` did. Were it
    /// the other way around (`B.id < A.id`), `B` would have been admitted
    /// to that queue first, and `A` — arriving later — would have had to
    /// queue behind `B` instead, contradicting `B` depending on `A`. So
    /// `A.id < B.id`. ∎
    ///
    /// This is what makes the worklist walk in `failed()` well-founded: it
    /// only ever looks *forward* (`drain(pos..)`, never backward) through a
    /// queue, so it can never re-visit or accidentally cross into something
    /// isolated — anything positioned before a reachable intent is, by this
    /// lemma, not reachable itself.
    ///
    /// ## Algorithm
    ///
    /// Given a failed intent `F` (validated to be at the front of every one
    /// of its own pubkey queues):
    ///
    /// 1. **Seed.** For each of `F`'s own pubkeys: mark it poisoned, and
    ///    remove its queue entirely. Everything behind `F` in that queue
    ///    (i.e. everything except `F` itself) is reachable from `F` by
    ///    definition — add it to the worklist.
    /// 2. **Propagate.** Pop an intent `V` from the worklist and remove it
    ///    from `blocked_intents` (skip if already removed — reachable via
    ///    another edge already processed). For each of `V`'s *other*
    ///    pubkeys: find `V`'s position in that queue (binary search — the
    ///    queue is sorted, per the lemma) and drain from that position to
    ///    the end. Everything drained besides `V` itself is newly
    ///    discovered as reachable — add it to the worklist. Mark the
    ///    pubkey poisoned. Record `V` as voided.
    /// 3. Repeat step 2 until the worklist is empty.
    ///
    /// Termination is immediate: each intent id is removed from
    /// `blocked_intents` at most once, so the worklist strictly shrinks.
    ///
    /// ## Why poisoning is unconditional
    ///
    /// A pubkey is marked poisoned as soon as a reachable intent touches
    /// it — even if, after the drain, isolated intents are still sitting
    /// in its queue and will go on to `complete()` successfully. This can
    /// look surprising (see the worked example: `a2` gets poisoned while
    /// `I0`/`I1` are still healthily using it), but the two things it's
    /// protecting are different:
    ///
    /// - Intents *already in the queue* are protected by voiding, which is
    ///   precise (isolated intents like `I1` are never touched, per the
    ///   lemma above).
    /// - Poisoning the key protects intents *not yet submitted*. A future
    ///   caller may construct a new intent on `a2` assuming the un-landed
    ///   chain's effects already happened (e.g. assuming `I3`'s deposit
    ///   landed before spending from `a2`). A simple balance check would
    ///   just fail cleanly on-chain if that assumption is wrong — but
    ///   intents can carry arbitrary actions/callbacks, and there's no
    ///   general guarantee that arbitrary program logic fails safely
    ///   against unexpected state. The scheduler can't tell in advance
    ///   which future intents are safe, so every key touched by a voided
    ///   intent is treated as unsafe until a human clears it (or the
    ///   process restarts).
    ///
    /// ## Worked example
    ///
    /// ```text
    /// I0 = [a1, a2]        executing, front of a1 and a2
    /// I2 = [b1, b2]        executing, front of b1 and b2  <- this one fails
    /// I1 = [a1, a2]        queued behind I0
    /// I3 = [a2, b1]        queued behind I1 on a2, behind I2 on b1
    /// I4 = [a1, a2]        queued behind I3 on a2, behind I1 on a1
    ///
    ///               a1   a2   b1   b2
    ///         pos0: I0   I0   I2   I2
    ///         pos1: I1   I1   I3    .
    ///         pos2: I4   I3    .    .
    ///         pos3:  .   I4    .    .
    /// ```
    ///
    /// `I2` fails. Seed: `poisoned_keys = {b1, b2}`, worklist = `{I3}`
    /// (`I2` itself is discarded, not voided — it already executed).
    ///
    /// Processing `I3` (`[a2, b1]`): `b1` is already poisoned, its queue is
    /// already gone. `a2`: `I3` is at position 2 in `[I0, I1, I3, I4]`;
    /// draining from there removes `I3, I4`, leaving `[I0, I1]`; `I4` goes
    /// to the worklist; `a2` is marked poisoned (even though `I0, I1`
    /// remain).
    ///
    /// Processing `I4` (`[a1, a2]`): `a2` already poisoned/drained, `I4` no
    /// longer there, skip. `a1`: `I4` is at position 2 in `[I0, I1, I4]`;
    /// draining removes just `I4`, leaving `[I0, I1]`; `a1` marked
    /// poisoned.
    ///
    /// Result: `poisoned_keys = {a1, a2, b1, b2}`, `poisoned = [I3, I4]`.
    /// `I0` and `I1` are never touched, remain in `blocked_keys`, and
    /// `complete()` normally — but `a1`/`a2` reject any *new* intent from
    /// here on, per "why poisoning is unconditional" above. `I1` is
    /// isolated from `I2`'s failure (no dependency chain reaches it — it's
    /// positioned *before* `I3` on `a2`, not after), which the lemma
    /// guarantees the drain-from-position walk can never touch.
    fn test_docs() {}
}

// Helper function to create test intents
#[cfg(test)]
pub(crate) fn create_test_intent(
    id: u64,
    pubkeys: &[Pubkey],
    is_undelegate: bool,
) -> OutboxIntentBundle {
    use magicblock_core::intent::{
        types::CommittedAccount, CommitAndUndelegate, CommitType,
        MagicIntentBundle, UndelegateType,
    };
    use magicblock_program::magic_scheduled_base_intent::ScheduledIntentBundle;
    use solana_account::Account;
    use solana_hash::Hash;
    use solana_transaction::Transaction;

    let mut intent = ScheduledIntentBundle {
        id,
        slot: 0,
        blockhash: Hash::default(),
        sent_transaction: Transaction::default(),
        payer: Pubkey::default(),
        intent_bundle: MagicIntentBundle::default(),
    };

    if !pubkeys.is_empty() {
        let committed_accounts = pubkeys
            .iter()
            .map(|&pubkey| CommittedAccount {
                pubkey,
                account: Account::default(),
                remote_slot: Default::default(),
            })
            .collect();

        let commit_type = CommitType::Standalone(committed_accounts);
        if is_undelegate {
            intent.intent_bundle.commit_and_undelegate =
                Some(CommitAndUndelegate {
                    commit_action: commit_type,
                    undelegate_action: UndelegateType::Standalone,
                })
        } else {
            intent.intent_bundle.commit = Some(commit_type);
        }
    }

    OutboxIntentBundle::accepted(intent)
}

#[cfg(test)]
pub(crate) fn create_test_intent_bundle(
    id: u64,
    commit_pubkeys: &[Pubkey],
    commit_and_undelegate_pubkeys: &[Pubkey],
) -> OutboxIntentBundle {
    use magicblock_core::intent::{
        types::CommittedAccount, CommitAndUndelegate, CommitType,
        MagicIntentBundle, UndelegateType,
    };
    use magicblock_program::magic_scheduled_base_intent::ScheduledIntentBundle;
    use solana_account::Account;
    use solana_hash::Hash;
    use solana_transaction::Transaction;

    let to_accounts = |keys: &[Pubkey]| -> Vec<CommittedAccount> {
        keys.iter()
            .copied()
            .map(|pubkey| CommittedAccount {
                pubkey,
                account: Account::default(),
                remote_slot: Default::default(),
            })
            .collect()
    };

    let mut intent = ScheduledIntentBundle {
        id,
        slot: 0,
        blockhash: Hash::default(),
        sent_transaction: Transaction::default(),
        payer: Pubkey::default(),
        intent_bundle: MagicIntentBundle::default(),
    };

    if !commit_pubkeys.is_empty() {
        intent.intent_bundle.commit =
            Some(CommitType::Standalone(to_accounts(commit_pubkeys)));
    }

    if !commit_and_undelegate_pubkeys.is_empty() {
        intent.intent_bundle.commit_and_undelegate =
            Some(CommitAndUndelegate {
                commit_action: CommitType::Standalone(to_accounts(
                    commit_and_undelegate_pubkeys,
                )),
                undelegate_action: UndelegateType::Standalone,
            });
    }

    OutboxIntentBundle::accepted(intent)
}
