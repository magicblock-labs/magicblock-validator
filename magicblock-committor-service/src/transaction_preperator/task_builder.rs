use dlp::args::{CallHandlerArgs, CommitStateArgs, CommitStateFromBufferArgs};
use magicblock_program::magic_scheduled_l1_message::{
    CommitAndUndelegate, CommitType, CommittedAccountV2, L1Action,
    MagicL1Message, ScheduledL1Message, UndelegateType,
};
use solana_pubkey::Pubkey;
use solana_sdk::instruction::{AccountMeta, Instruction};

// pub trait PossibleTaskTrait {
//     fn instruction() -> Instruction;
//     fn decrease(self: Box<Self>) -> Result<Box<dyn PossibleTaskTrait>, Box<dyn PossibleTaskTrait>>;
//     // If task is "preparable" returns Instructions for preparations
//     fn prepare(self: Box<Self>) -> Option<Vec<Instruction>>;
// }

#[derive(Clone)]
pub struct CommitTask {
    pub allow_undelegatio: bool,
    pub committed_account: CommittedAccountV2,
}

#[derive(Clone)]
pub struct UndelegateTask {
    pub delegated_account: Pubkey,
    pub owner_program: Pubkey,
    pub rent_reimbursement: Pubkey,
}

#[derive(Clone)]
pub struct FinalizeTask {
    pub delegated_account: Pubkey,
}

#[derive(Clone)]
pub enum Task {
    Commit(CommitTask),
    Finalize(FinalizeTask), // TODO(edwin): introduce Stages instead?
    Undelegate(UndelegateTask), // Special action really
    L1Action(L1Action),
}

impl Task {
    pub fn is_bufferable(&self) -> bool {
        match self {
            Self::Commit(_) => true,
            Self::Finalize(_) => false,
            Self::Undelegate(_) => false,
            Self::L1Action(_) => false, // TODO(edwin): enable
        }
    }

    pub fn args_instruction(
        &self,
        validator: Pubkey,
        commit_id: u64,
    ) -> Instruction {
        match self {
            Task::Commit(value) => {
                let args = CommitStateArgs {
                    slot: commit_id, // TODO(edwin): change slot,
                    lamports: value.committed_account.account.lamports,
                    data: value.committed_account.account.data.clone(),
                    allow_undelegation: value.allow_undelegatio, // TODO(edwin):
                };
                dlp::instruction_builder::commit_state(
                    validator,
                    value.committed_account.pubkey,
                    value.committed_account.account.owner,
                    args,
                )
            }
            Task::Finalize(value) => dlp::instruction_builder::finalize(
                validator,
                value.delegated_account,
            ),
            Task::Undelegate(value) => dlp::instruction_builder::undelegate(
                validator,
                value.delegated_account,
                value.owner_program,
                value.rent_reimbursement,
            ),
            Task::L1Action(value) => {
                let account_metas = value
                    .account_metas_per_program
                    .iter()
                    .map(|short_meta| AccountMeta {
                        pubkey: short_meta.pubkey,
                        is_writable: short_meta.is_writable,
                        is_signer: false,
                    })
                    .collect();
                dlp::instruction_builder::call_handler(
                    validator,
                    value.destination_program,
                    value.escrow_authority,
                    account_metas,
                    CallHandlerArgs {
                        data: value.data_per_program.data.clone(),
                        escrow_index: value.data_per_program.escrow_index,
                    },
                )
            }
        }
        todo!()
    }

    pub fn buffer_instruction(
        &self,
        validator: Pubkey,
        commit_id: u64,
    ) -> Instruction {
        // TODO(edwin): now this is bad, while impossible
        // We should use dyn Task
        match self {
            Task::Commit(value) => {
                let commit_id_slice = commit_id.to_le_bytes();
                let (commit_buffer_pubkey, _) =
                    magicblock_committor_program::pdas::chunks_pda(
                        &validator,
                        &value.committed_account.pubkey,
                        &commit_id_slice,
                    );
                dlp::instruction_builder::commit_state_from_buffer(
                    validator,
                    value.committed_account.pubkey,
                    value.committed_account.account.owner,
                    commit_buffer_pubkey,
                    CommitStateFromBufferArgs {
                        slot: commit_id, //TODO(edwin): change to commit_id
                        lamports: value.committed_account.account.lamports,
                        allow_undelegation: value.allow_undelegatio,
                    },
                )
            }
            Task::Undelegate(_) => unreachable!(),
            Task::Finalize(_) => unreachable!(),
            Task::L1Action(_) => unreachable!(), // TODO(edwin): enable
        }
    }
}

pub trait TasksBuilder {
    // Creates tasks for commit stage
    fn commit_tasks(l1_message: &ScheduledL1Message) -> Vec<Task>;

    // Create tasks for finalize stage
    fn finalize_tasks(l1_message: &ScheduledL1Message) -> Vec<Task>;
}

/// V1 Task builder
/// V1: Actions are part of finalize tx
pub struct TaskBuilderV1;
impl TasksBuilder for TaskBuilderV1 {
    /// Returns [`Task`]s for Commit stage
    fn commit_tasks(l1_message: &ScheduledL1Message) -> Vec<Task> {
        let accounts = match &l1_message.l1_message {
            MagicL1Message::L1Actions(actions) => {
                return actions
                    .into_iter()
                    .map(|el| Task::L1Action(el.clone()))
                    .collect()
            }
            MagicL1Message::Commit(t) => t.get_committed_accounts(),
            MagicL1Message::CommitAndUndelegate(t) => {
                t.commit_action.get_committed_accounts()
            }
        };

        accounts
            .into_iter()
            .map(|account| Task::Commit(account.clone()))
            .collect()
    }

    /// Returns [`Task`]s for Finalize stage
    fn finalize_tasks(l1_message: &ScheduledL1Message) -> Vec<Task> {
        fn commit_type_tasks(value: &CommitType) -> Vec<Task> {
            match value {
                CommitType::Standalone(accounts) => accounts
                    .into_iter()
                    .map(|account| Task::Finalize(account.clone()))
                    .collect(),
                CommitType::WithL1Actions {
                    committed_accounts,
                    l1_actions,
                } => {
                    let mut tasks = committed_accounts
                        .into_iter()
                        .map(|account| Task::Finalize(account.clone()))
                        .collect::<Vec<Task>>();
                    tasks.extend(
                        l1_actions
                            .into_iter()
                            .map(|action| Task::L1Action(action.clone())),
                    );
                    tasks
                }
            }
        }

        // TODO(edwin): improve, separate into smaller pieces. Maybe Visitor?
        match &l1_message.l1_message {
            MagicL1Message::L1Actions(_) => panic!("enable"), // TODO(edwin)
            MagicL1Message::Commit(value) => commit_type_tasks(value),
            MagicL1Message::CommitAndUndelegate(t) => {
                let mut commit_tasks = commit_type_tasks(&t.commit_action);
                match &t.undelegate_action {
                    UndelegateType::Standalone => {
                        let accounts = t.get_committed_accounts();
                        commit_tasks.extend(
                            accounts.into_iter().map(|account| {
                                Task::Undelegate(account.clone())
                            }),
                        );
                    }
                    UndelegateType::WithL1Actions(actions) => {
                        // tasks example: [Finalize(Acc1), Action, Undelegate(Acc1), Action]
                        let accounts = t.get_committed_accounts();
                        commit_tasks.extend(
                            accounts.into_iter().map(|account| {
                                Task::Undelegate(account.clone())
                            }),
                        );
                        commit_tasks.extend(
                            actions
                                .into_iter()
                                .map(|action| Task::L1Action(action.clone())),
                        );
                    }
                };

                commit_tasks
            }
        }
    }
}
