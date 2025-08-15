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
    magic_context::MagicContext,
    magic_scheduled_base_intent::ScheduledBaseIntent,
};

#[derive(Clone)]
pub struct TransactionScheduler {
    scheduled_base_intents: Arc<RwLock<Vec<ScheduledBaseIntent>>>,
}

impl Default for TransactionScheduler {
    fn default() -> Self {
        lazy_static! {
            /// This vec tracks commits that went through the entire process of first
            /// being scheduled into the MagicContext, and then being moved
            /// over to this global.
            static ref SCHEDULED_ACTION: Arc<RwLock<Vec<ScheduledBaseIntent >>> =
                Default::default();
        }
        Self {
            scheduled_base_intents: SCHEDULED_ACTION.clone(),
        }
    }
}

impl TransactionScheduler {
    pub fn schedule_base_intent(
        invoke_context: &InvokeContext,
        context_account: &RefCell<AccountSharedData>,
        action: ScheduledBaseIntent,
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

    pub fn accept_scheduled_base_intent(
        &self,
        base_intents: Vec<ScheduledBaseIntent>,
    ) {
        self.scheduled_base_intents
            .write()
            .expect("scheduled_action lock poisoned")
            .extend(base_intents);
    }

    pub fn get_scheduled_actions_by_payer(
        &self,
        payer: &Pubkey,
    ) -> Vec<ScheduledBaseIntent> {
        let commits = self
            .scheduled_base_intents
            .read()
            .expect("scheduled_action lock poisoned");

        commits
            .iter()
            .filter(|x| x.payer.eq(payer))
            .cloned()
            .collect::<Vec<_>>()
    }

    pub fn take_scheduled_actions(&self) -> Vec<ScheduledBaseIntent> {
        let mut lock = self
            .scheduled_base_intents
            .write()
            .expect("scheduled_action lock poisoned");
        mem::take(&mut *lock)
    }

    pub fn scheduled_actions_len(&self) -> usize {
        let lock = self
            .scheduled_base_intents
            .read()
            .expect("scheduled_action lock poisoned");

        lock.len()
    }

    pub fn clear_scheduled_actions(&self) {
        let mut lock = self
            .scheduled_base_intents
            .write()
            .expect("scheduled_action lock poisoned");
        lock.clear();
    }
}
