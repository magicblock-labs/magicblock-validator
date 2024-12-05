use std::{
    cell::RefCell,
    mem,
    sync::{Arc, RwLock},
};

use lazy_static::lazy_static;
use solana_program_runtime::{ic_msg, invoke_context::InvokeContext};
use solana_sdk::{
    account::AccountSharedData, account_utils::StateMut,
    instruction::InstructionError, pubkey::Pubkey,
};

use crate::magic_context::{MagicContext, ScheduledCommit};

#[derive(Clone)]
pub struct TransactionScheduler {
    scheduled_commits: Arc<RwLock<Vec<ScheduledCommit>>>,
}

impl Default for TransactionScheduler {
    fn default() -> Self {
        lazy_static! {
            /// This vec tracks commits that went through the entire process of first
            /// being scheduled into the MagicContext, and then being moved
            /// over to this global.
            static ref SCHEDULED_COMMITS: Arc<RwLock<Vec<ScheduledCommit>>> =
                Default::default();
        }
        Self {
            scheduled_commits: SCHEDULED_COMMITS.clone(),
        }
    }
}

impl TransactionScheduler {
    pub fn schedule_commit(
        invoke_context: &InvokeContext,
        context_account: &RefCell<AccountSharedData>,
        commit: ScheduledCommit,
    ) -> Result<(), InstructionError> {
        let context_data = &mut context_account.borrow_mut();
        let mut context =
            MagicContext::deserialize(context_data).map_err(|err| {
                ic_msg!(
                    invoke_context,
                    "Failed to deserialize MagicContext: {}",
                    err
                );
                InstructionError::GenericError
            })?;
        context.add_scheduled_commit(commit);
        context_data.set_state(&context)?;
        Ok(())
    }

    pub fn accept_scheduled_commits(&self, commits: Vec<ScheduledCommit>) {
        self.scheduled_commits
            .write()
            .expect("scheduled_commits lock poisoned")
            .extend(commits);
    }

    pub fn get_scheduled_commits_by_payer(
        &self,
        payer: &Pubkey,
    ) -> Vec<ScheduledCommit> {
        let commits = self
            .scheduled_commits
            .read()
            .expect("scheduled_commits lock poisoned");

        commits
            .iter()
            .filter(|x| x.payer.eq(payer))
            .cloned()
            .collect::<Vec<_>>()
    }

    pub fn take_scheduled_commits(&self) -> Vec<ScheduledCommit> {
        let mut lock = self
            .scheduled_commits
            .write()
            .expect("scheduled_commits lock poisoned");
        mem::take(&mut *lock)
    }

    pub fn scheduled_commits_len(&self) -> usize {
        let lock = self
            .scheduled_commits
            .read()
            .expect("scheduled_commits lock poisoned");

        lock.len()
    }

    pub fn clear_scheduled_commits(&self) {
        let mut lock = self
            .scheduled_commits
            .write()
            .expect("scheduled_commits lock poisoned");
        lock.clear();
    }
}
