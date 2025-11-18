use std::sync::Arc;

use magicblock_program::magic_scheduled_base_intent::CommittedAccount;

use super::{CommitStrategy, CommitTask};
use crate::intent_executor::task_info_fetcher::TaskInfoFetcher;

pub struct CommitTaskBuilder;

impl CommitTaskBuilder {
    pub async fn create_commit_task<C: TaskInfoFetcher>(
        commit_id: u64,
        allow_undelegation: bool,
        committed_account: CommittedAccount,
        task_info_fetcher: &Arc<C>,
    ) -> CommitTask {
        let base_account = if committed_account.account.data.len()
            > CommitTask::COMMIT_STATE_SIZE_THRESHOLD
        {
            match task_info_fetcher
                .get_base_account(&committed_account.pubkey)
                .await
            {
                Ok(Some(account)) => Some(account),
                Ok(None) => {
                    log::warn!("AccountNotFound for commit_diff, pubkey: {}, commit_id: {}, Falling back to commit_state.",
                        committed_account.pubkey, commit_id);
                    None
                }
                Err(e) => {
                    log::warn!("Failed to fetch base account for commit diff, pubkey: {}, commit_id: {}, error: {}. Falling back to commit_state.",
                        committed_account.pubkey, commit_id, e);
                    None
                }
            }
        } else {
            None
        };

        CommitTask {
            commit_id,
            allow_undelegation,
            committed_account,
            strategy: match base_account {
                Some(base_account) => {
                    CommitStrategy::DiffInArgs { base_account }
                }
                None => CommitStrategy::StateInArgs,
            },
        }
    }
}
