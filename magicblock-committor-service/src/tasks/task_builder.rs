use std::collections::HashMap;

use dlp::args::Context;
use magicblock_program::magic_scheduled_l1_message::{
    CommitType, CommittedAccountV2, MagicL1Message, ScheduledL1Message,
    UndelegateType,
};
use solana_pubkey::Pubkey;

use crate::tasks::tasks::{
    ArgsTask, CommitTask, FinalizeTask, L1ActionTask, L1Task, UndelegateTask,
};

pub trait TasksBuilder {
    // Creates tasks for commit stage
    fn commit_tasks(
        l1_message: &ScheduledL1Message,
        commit_ids: &HashMap<Pubkey, u64>,
    ) -> TaskBuilderResult<Vec<Box<dyn L1Task>>>;

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
        commit_ids: &HashMap<Pubkey, u64>,
    ) -> TaskBuilderResult<Vec<Box<dyn L1Task>>> {
        let (accounts, allow_undelegation) = match &l1_message.l1_message {
            MagicL1Message::L1Actions(actions) => {
                let tasks = actions
                    .into_iter()
                    .map(|el| {
                        let task = L1ActionTask {
                            context: Context::Standalone,
                            action: el.clone(),
                        };
                        Box::new(ArgsTask::L1Action(task)) as Box<dyn L1Task>
                    })
                    .collect();
                return Ok(tasks);
            }
            MagicL1Message::Commit(t) => (t.get_committed_accounts(), false),
            MagicL1Message::CommitAndUndelegate(t) => {
                (t.commit_action.get_committed_accounts(), true)
            }
        };

        let tasks = accounts
            .into_iter()
            .map(|account| {
                if let Some(commit_id) = commit_ids.get(&account.pubkey) {
                    Ok(Box::new(ArgsTask::Commit(CommitTask {
                        commit_id: *commit_id + 1,
                        allow_undelegation,
                        committed_account: account.clone(),
                    })) as Box<dyn L1Task>)
                } else {
                    Err(Error::MissingCommitIdError(account.pubkey))
                }
            })
            .collect::<Result<_, _>>()?;

        Ok(tasks)
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
                    tasks.extend(l1_actions.iter().map(|action| {
                        let task = L1ActionTask {
                            context: Context::Commit,
                            action: action.clone(),
                        };
                        Box::new(ArgsTask::L1Action(task)) as Box<dyn L1Task>
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
                        tasks.extend(actions.iter().map(|action| {
                            let task = L1ActionTask {
                                context: Context::Undelegate,
                                action: action.clone(),
                            };
                            Box::new(ArgsTask::L1Action(task))
                                as Box<dyn L1Task>
                        }));
                    }
                }

                tasks
            }
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("Missing commit id for pubkey: {0}")]
    MissingCommitIdError(Pubkey),
}

pub type TaskBuilderResult<T, E = Error> = Result<T, E>;
