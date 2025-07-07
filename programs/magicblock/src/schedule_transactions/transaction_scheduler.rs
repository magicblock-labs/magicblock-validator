use std::{
    cell::RefCell,
    mem,
    sync::{Arc, RwLock},
};

use lazy_static::lazy_static;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_sdk::{
    account::AccountSharedData, account_utils::StateMut,
    instruction::InstructionError, pubkey::Pubkey,
};

use crate::{
    magic_context::MagicContext, magic_scheduled_l1_message::ScheduledL1Message,
};

#[derive(Clone)]
pub struct TransactionScheduler {
    scheduled_l1_message: Arc<RwLock<Vec<ScheduledL1Message>>>,
}

impl Default for TransactionScheduler {
    fn default() -> Self {
        lazy_static! {
            /// This vec tracks commits that went through the entire process of first
            /// being scheduled into the MagicContext, and then being moved
            /// over to this global.
            static ref SCHEDULED_ACTION: Arc<RwLock<Vec<ScheduledL1Message >>> =
                Default::default();
        }
        Self {
            scheduled_l1_message: SCHEDULED_ACTION.clone(),
        }
    }
}

impl TransactionScheduler {
    pub fn schedule_l1_message(
        invoke_context: &InvokeContext,
        context_account: &RefCell<AccountSharedData>,
        action: ScheduledL1Message,
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
        context.add_scheduled_action(action);
        context_data.set_state(&context)?;
        Ok(())
    }

    pub fn accept_scheduled_l1_message(
        &self,
        commits: Vec<ScheduledL1Message>,
    ) {
        self.scheduled_l1_message
            .write()
            .expect("scheduled_action lock poisoned")
            .extend(commits);
    }

    pub fn get_scheduled_actions_by_payer(
        &self,
        payer: &Pubkey,
    ) -> Vec<ScheduledL1Message> {
        let commits = self
            .scheduled_l1_message
            .read()
            .expect("scheduled_action lock poisoned");

        commits
            .iter()
            .filter(|x| x.payer.eq(payer))
            .cloned()
            .collect::<Vec<_>>()
    }

    pub fn take_scheduled_actions(&self) -> Vec<ScheduledL1Message> {
        let mut lock = self
            .scheduled_l1_message
            .write()
            .expect("scheduled_action lock poisoned");
        mem::take(&mut *lock)
    }

    pub fn scheduled_actions_len(&self) -> usize {
        let lock = self
            .scheduled_l1_message
            .read()
            .expect("scheduled_action lock poisoned");

        lock.len()
    }

    pub fn clear_scheduled_actions(&self) {
        let mut lock = self
            .scheduled_l1_message
            .write()
            .expect("scheduled_action lock poisoned");
        lock.clear();
    }
}
