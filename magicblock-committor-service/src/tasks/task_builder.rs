use std::sync::Arc;

use dlp::{args::Context, state::DelegationMetadata};
use log::error;
use magicblock_program::magic_scheduled_base_intent::{
    CommitType, CommittedAccountV2, MagicBaseIntent, ScheduledBaseIntent,
    UndelegateType,
};
use magicblock_rpc_client::{MagicBlockRpcClientError, MagicblockRpcClient};
use solana_pubkey::Pubkey;

use crate::{
    intent_executor::commit_id_fetcher::CommitIdFetcher,
    persist::IntentPersister,
    tasks::tasks::{
        ArgsTask, BaseTask, CommitTask, FinalizeTask, L1ActionTask,
        UndelegateTask,
    },
};

#[async_trait::async_trait]
pub trait TasksBuilder {
    // Creates tasks for commit stage
    async fn commit_tasks<C: CommitIdFetcher, P: IntentPersister>(
        commit_id_fetcher: &Arc<C>,
        l1_message: &ScheduledBaseIntent,
        persister: &Option<P>,
    ) -> TaskBuilderResult<Vec<Box<dyn BaseTask>>>;

    // Create tasks for finalize stage
    async fn finalize_tasks(
        rpc_client: &MagicblockRpcClient,
        l1_message: &ScheduledBaseIntent,
    ) -> TaskBuilderResult<Vec<Box<dyn BaseTask>>>;
}

/// V1 Task builder
/// V1: Actions are part of finalize tx
pub struct TaskBuilderV1;

impl TaskBuilderV1 {
    async fn fetch_rent_reimbursements(
        rpc_client: &MagicblockRpcClient,
        pubkeys: &[Pubkey],
    ) -> Result<Vec<Pubkey>, FinalizedTasksBuildError> {
        let pdas = pubkeys
            .iter()
            .map(|pubkey| {
                dlp::pda::delegation_metadata_pda_from_delegated_account(pubkey)
            })
            .collect::<Vec<_>>();

        let metadatas = rpc_client.get_multiple_accounts(&pdas, None).await?;

        let rent_reimbursments = pdas
            .into_iter()
            .enumerate()
            .map(|(i, pda)| {
                let account = if let Some(account) = metadatas.get(i) {
                    account
                } else {
                    return Err(
                        FinalizedTasksBuildError::MetadataNotFoundError(pda),
                    );
                };

                let account = account.as_ref().ok_or(
                    FinalizedTasksBuildError::MetadataNotFoundError(pda),
                )?;
                let metadata =
                    DelegationMetadata::try_from_bytes_with_discriminator(
                        &account.data,
                    )
                    .map_err(|_| {
                        FinalizedTasksBuildError::InvalidAccountDataError(pda)
                    })?;

                Ok::<_, FinalizedTasksBuildError>(metadata.rent_payer)
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(rent_reimbursments)
    }
}

#[async_trait::async_trait]
impl TasksBuilder for TaskBuilderV1 {
    /// Returns [`Task`]s for Commit stage
    async fn commit_tasks<C: CommitIdFetcher, P: IntentPersister>(
        commit_id_fetcher: &Arc<C>,
        l1_message: &ScheduledBaseIntent,
        persister: &Option<P>,
    ) -> TaskBuilderResult<Vec<Box<dyn BaseTask>>> {
        let (accounts, allow_undelegation) = match &l1_message.base_intent {
            MagicBaseIntent::BaseActions(actions) => {
                let tasks = actions
                    .into_iter()
                    .map(|el| {
                        let task = L1ActionTask {
                            context: Context::Standalone,
                            action: el.clone(),
                        };
                        Box::new(ArgsTask::L1Action(task)) as Box<dyn BaseTask>
                    })
                    .collect();

                return Ok(tasks);
            }
            MagicBaseIntent::Commit(t) => (t.get_committed_accounts(), false),
            MagicBaseIntent::CommitAndUndelegate(t) => {
                (t.commit_action.get_committed_accounts(), true)
            }
        };

        let committed_pubkeys = accounts
            .iter()
            .map(|account| account.pubkey)
            .collect::<Vec<_>>();
        let commit_ids = commit_id_fetcher
            .fetch_commit_ids(&committed_pubkeys)
            .await?;

        // Persist commit ids for commitees
        commit_ids
            .iter()
            .for_each(|(pubkey, commit_id) | {
                if let Err(err) = persister.set_commit_id(l1_message.id, pubkey, *commit_id) {
                    error!("Failed to persist commit id: {}, for message id: {} with pubkey {}: {}", commit_id, l1_message.id, pubkey, err);
                }
            });

        let tasks = accounts
            .into_iter()
            .map(|account| {
                let commit_id = commit_ids.get(&account.pubkey).expect("CommitIdFetcher provide commit ids for all listed pubkeys, or errors!");
                let task = ArgsTask::Commit(CommitTask {
                    commit_id: *commit_id + 1,
                    allow_undelegation,
                    committed_account: account.clone(),
                });

                Box::new(task) as Box<dyn BaseTask>
            })
            .collect();

        Ok(tasks)
    }

    /// Returns [`Task`]s for Finalize stage
    async fn finalize_tasks(
        rpc_client: &MagicblockRpcClient,
        l1_message: &ScheduledBaseIntent,
    ) -> TaskBuilderResult<Vec<Box<dyn BaseTask>>> {
        // Helper to create a finalize task
        fn finalize_task(account: &CommittedAccountV2) -> Box<dyn BaseTask> {
            Box::new(ArgsTask::Finalize(FinalizeTask {
                delegated_account: account.pubkey,
            }))
        }

        // Helper to create an undelegate task
        fn undelegate_task(
            account: &CommittedAccountV2,
            rent_reimbursement: &Pubkey,
        ) -> Box<dyn BaseTask> {
            Box::new(ArgsTask::Undelegate(UndelegateTask {
                delegated_account: account.pubkey,
                owner_program: account.account.owner,
                rent_reimbursement: *rent_reimbursement,
            }))
        }

        // Helper to process commit types
        fn process_commit(commit: &CommitType) -> Vec<Box<dyn BaseTask>> {
            match commit {
                CommitType::Standalone(accounts) => {
                    accounts.iter().map(finalize_task).collect()
                }
                CommitType::WithBaseActions {
                    committed_accounts,
                    base_actions: l1_actions,
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
                        Box::new(ArgsTask::L1Action(task)) as Box<dyn BaseTask>
                    }));
                    tasks
                }
            }
        }

        match &l1_message.base_intent {
            MagicBaseIntent::BaseActions(_) => Ok(vec![]),
            MagicBaseIntent::Commit(commit) => Ok(process_commit(commit)),
            MagicBaseIntent::CommitAndUndelegate(t) => {
                let mut tasks = process_commit(&t.commit_action);

                // Get rent reimbursments for undelegated accounts
                let accounts = t.get_committed_accounts();
                let pubkeys = accounts
                    .iter()
                    .map(|account| account.pubkey)
                    .collect::<Vec<_>>();
                let rent_reimbursments =
                    Self::fetch_rent_reimbursements(rpc_client, &pubkeys)
                        .await?;
                tasks.extend(accounts.iter().zip(rent_reimbursments).map(
                    |(account, rent_reimbursement)| {
                        undelegate_task(account, &rent_reimbursement)
                    },
                ));

                match &t.undelegate_action {
                    UndelegateType::Standalone => Ok(tasks),
                    UndelegateType::WithBaseActions(actions) => {
                        tasks.extend(actions.iter().map(|action| {
                            let task = L1ActionTask {
                                context: Context::Undelegate,
                                action: action.clone(),
                            };
                            Box::new(ArgsTask::L1Action(task))
                                as Box<dyn BaseTask>
                        }));

                        Ok(tasks)
                    }
                }
            }
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum FinalizedTasksBuildError {
    #[error("Metadata not found for: {0}")]
    MetadataNotFoundError(Pubkey),
    #[error("InvalidAccountDataError for: {0}")]
    InvalidAccountDataError(Pubkey),
    #[error("MagicBlockRpcClientError: {0}")]
    MagicBlockRpcClientError(#[from] MagicBlockRpcClientError),
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("CommitIdFetchError: {0}")]
    CommitTasksBuildError(
        #[from] crate::intent_executor::commit_id_fetcher::Error,
    ),
    #[error("FinalizedTasksBuildError: {0}")]
    FinalizedTasksBuildError(#[from] FinalizedTasksBuildError),
}

pub type TaskBuilderResult<T, E = Error> = Result<T, E>;
