use std::{cell::RefCell, collections::VecDeque, mem};

use magicblock_magic_program_api::args::TaskRequest;
use solana_pubkey::Pubkey;

use crate::intent::ScheduledIntentBundle;

#[derive(Default, Debug)]
pub struct ExecutionTlsStash {
    tasks: VecDeque<TaskRequest>,
    newly_created_magic_atas: VecDeque<Pubkey>,
    intents: Vec<ScheduledIntentBundle>,
}

thread_local! {
    static EXECUTION_TLS_STASH: RefCell<ExecutionTlsStash> = RefCell::default();
}

impl ExecutionTlsStash {
    pub fn register_task(task: TaskRequest) {
        EXECUTION_TLS_STASH
            .with_borrow_mut(|stash| stash.tasks.push_back(task));
    }

    pub fn next_task() -> Option<TaskRequest> {
        EXECUTION_TLS_STASH.with_borrow_mut(|stash| stash.tasks.pop_front())
    }

    pub fn register_newly_created_magic_ata(pubkey: Pubkey) {
        EXECUTION_TLS_STASH.with_borrow_mut(|stash| {
            stash.newly_created_magic_atas.push_back(pubkey)
        });
    }

    pub fn pop_newly_created_magic_ata() -> Option<Pubkey> {
        EXECUTION_TLS_STASH
            .with_borrow_mut(|stash| stash.newly_created_magic_atas.pop_front())
    }

    /// Stages accepted intents until the enclosing transaction commits.
    pub fn register_scheduled_intent_bundles(
        intents: Vec<ScheduledIntentBundle>,
    ) {
        EXECUTION_TLS_STASH.with_borrow_mut(|stash| {
            if stash.intents.is_empty() {
                stash.intents = intents;
            } else {
                stash.intents.extend(intents);
            }
        });
    }

    pub fn take_scheduled_intent_bundles() -> Vec<ScheduledIntentBundle> {
        EXECUTION_TLS_STASH
            .with_borrow_mut(|stash| mem::take(&mut stash.intents))
    }

    pub fn clear() {
        EXECUTION_TLS_STASH.with_borrow_mut(|stash| {
            stash.tasks.clear();
            stash.newly_created_magic_atas.clear();
            stash.intents.clear();
        })
    }
}

#[cfg(test)]
mod tests {
    use solana_account::Account;

    use super::*;
    use crate::intent::{CommitType, CommittedAccount, MagicIntentBundle};

    fn intent(id: u64) -> ScheduledIntentBundle {
        ScheduledIntentBundle {
            id,
            slot: 1,
            blockhash: Default::default(),
            sent_transaction: Default::default(),
            payer: Pubkey::new_unique(),
            intent_bundle: MagicIntentBundle {
                commit: Some(CommitType::Standalone(vec![CommittedAccount {
                    pubkey: Pubkey::new_unique(),
                    account: Account {
                        data: vec![42; 4096],
                        ..Default::default()
                    },
                    remote_slot: 1,
                }])),
                ..Default::default()
            },
        }
    }

    #[test]
    fn scheduled_intent_batches_move_in_order_and_clear() {
        ExecutionTlsStash::clear();
        let batch = vec![intent(1), intent(2)];
        let batch_ptr = batch.as_ptr();
        let payload_ptr = batch[0].get_commit_intent_accounts().unwrap()[0]
            .account
            .data
            .as_ptr();

        ExecutionTlsStash::register_scheduled_intent_bundles(batch);
        let batch = ExecutionTlsStash::take_scheduled_intent_bundles();
        assert_eq!(batch.as_ptr(), batch_ptr);
        assert!(ExecutionTlsStash::take_scheduled_intent_bundles().is_empty());

        ExecutionTlsStash::register_scheduled_intent_bundles(batch);
        ExecutionTlsStash::register_scheduled_intent_bundles(vec![intent(3)]);
        let batch = ExecutionTlsStash::take_scheduled_intent_bundles();
        assert_eq!(
            batch.iter().map(|bundle| bundle.id).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(
            batch[0].get_commit_intent_accounts().unwrap()[0]
                .account
                .data
                .as_ptr(),
            payload_ptr
        );
        assert!(ExecutionTlsStash::take_scheduled_intent_bundles().is_empty());

        ExecutionTlsStash::register_scheduled_intent_bundles(batch);
        ExecutionTlsStash::clear();
        assert!(ExecutionTlsStash::take_scheduled_intent_bundles().is_empty());
    }
}
