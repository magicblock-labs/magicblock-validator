use crate::magic_context::{MagicContext, ScheduledCommit};
use lazy_static::lazy_static;
use solana_sdk::{account::AccountSharedData, pubkey::Pubkey};
use std::cell::RefCell;
use std::{
    mem,
    sync::{Arc, RwLock},
};

#[derive(Clone)]
pub struct TransactionScheduler {
    scheduled_commits: Arc<RwLock<Vec<ScheduledCommit>>>,
}

impl Default for TransactionScheduler {
    fn default() -> Self {
        lazy_static! {
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
        &self,
        context_account: &RefCell<AccountSharedData>,
        commit: ScheduledCommit,
    ) -> Result<(), bincode::Error> {
        let context_data = context_account.borrow_mut();
        let mut context = context_data.deserialize_data::<MagicContext>()?;
        context.add_scheduled_commit(commit);
        Ok(())
    }

    pub fn accept_scheduled_commits(&self) {
        todo!()
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
}
