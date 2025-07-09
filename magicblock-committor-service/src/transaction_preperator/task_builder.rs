use std::collections::HashMap;

use magicblock_program::magic_scheduled_l1_message::{
    CommitType, CommittedAccountV2, L1Action, MagicL1Message,
    ScheduledL1Message, UndelegateType,
};
use solana_pubkey::Pubkey;

use crate::transaction_preperator::tasks::{
    ArgsTask, CommitTask, FinalizeTask, L1Task, TaskPreparationInfo,
    UndelegateTask,
};

pub trait TasksBuilder {
    // Creates tasks for commit stage
    fn commit_tasks(
        l1_message: &ScheduledL1Message,
        commit_ids: HashMap<Pubkey, u64>,
    ) -> Vec<Box<dyn L1Task>>;

    // Create tasks for finalize stage
    fn finalize_tasks(
        l1_message: &ScheduledL1Message,
        rent_reimbursement: &Pubkey,
    ) -> Vec<Box<dyn L1Task>>;
}

/// V1 Task builder
/// V1: Actions are part of finalize tx
pub struct TaskBuilderV1;
impl TasksBuilder for TaskBuilderV1 {
    /// Returns [`Task`]s for Commit stage
    fn commit_tasks(
        l1_message: &ScheduledL1Message,
        commit_ids: HashMap<Pubkey, u64>,
    ) -> Vec<Box<dyn L1Task>> {
        let (accounts, allow_undelegation) = match &l1_message.l1_message {
            MagicL1Message::L1Actions(actions) => {
                return actions
                    .into_iter()
                    .map(|el| {
                        Box::new(ArgsTask::L1Action(el.clone()))
                            as Box<dyn L1Task>
                    })
                    .collect()
            }
            MagicL1Message::Commit(t) => (t.get_committed_accounts(), false),
            MagicL1Message::CommitAndUndelegate(t) => {
                (t.commit_action.get_committed_accounts(), true)
            }
        };

        accounts
            .into_iter()
            .map(|account| {
                if let Some(commit_id) = commit_ids.get(&account.pubkey) {
                    Ok(Box::new(ArgsTask::Commit(CommitTask {
                        commit_id: *commit_id + 1,
                        allow_undelegation,
                        committed_account: account.clone(),
                    })) as Box<dyn L1Task>)
                } else {
                    // TODO(edwin): proper error
                    Err(())
                }
            })
            .collect::<Result<_, _>>()
            .unwrap() // TODO(edwin): remove
    }

    /// Returns [`Task`]s for Finalize stage
    fn finalize_tasks(
        l1_message: &ScheduledL1Message,
        rent_reimbursement: &Pubkey,
    ) -> Vec<Box<dyn L1Task>> {
        // Helper to create a finalize task
        fn finalize_task(account: &CommittedAccountV2) -> Box<dyn L1Task> {
            Box::new(ArgsTask::Finalize(FinalizeTask {
                delegated_account: account.pubkey,
            }))
        }

        // Helper to create an undelegate task
        fn undelegate_task(
            account: &CommittedAccountV2,
            rent_reimbursement: &Pubkey,
        ) -> Box<dyn L1Task> {
            Box::new(ArgsTask::Undelegate(UndelegateTask {
                delegated_account: account.pubkey,
                owner_program: account.account.owner,
                rent_reimbursement: *rent_reimbursement,
            }))
        }

        // Helper to process commit types
        fn process_commit(commit: &CommitType) -> Vec<Box<dyn L1Task>> {
            match commit {
                CommitType::Standalone(accounts) => {
                    accounts.iter().map(finalize_task).collect()
                }
                CommitType::WithL1Actions {
                    committed_accounts,
                    l1_actions,
                } => {
                    let mut tasks = committed_accounts
                        .iter()
                        .map(finalize_task)
                        .collect::<Vec<_>>();
                    tasks.extend(l1_actions.iter().map(|a| {
                        Box::new(ArgsTask::L1Action(a.clone()))
                            as Box<dyn L1Task>
                    }));
                    tasks
                }
            }
        }

        match &l1_message.l1_message {
            MagicL1Message::L1Actions(_) => vec![],
            MagicL1Message::Commit(commit) => process_commit(commit),
            MagicL1Message::CommitAndUndelegate(t) => {
                let mut tasks = process_commit(&t.commit_action);
                let accounts = t.get_committed_accounts();

                match &t.undelegate_action {
                    UndelegateType::Standalone => {
                        tasks.extend(
                            accounts.iter().map(|a| {
                                undelegate_task(a, rent_reimbursement)
                            }),
                        );
                    }
                    UndelegateType::WithL1Actions(actions) => {
                        tasks.extend(
                            accounts.iter().map(|a| {
                                undelegate_task(a, rent_reimbursement)
                            }),
                        );
                        tasks.extend(actions.iter().map(|a| {
                            Box::new(ArgsTask::L1Action(a.clone()))
                                as Box<dyn L1Task>
                        }));
                    }
                }

                tasks
            }
        }
    }
}
