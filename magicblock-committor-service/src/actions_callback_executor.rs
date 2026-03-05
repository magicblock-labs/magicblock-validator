use magicblock_core::{
    link::transactions::TransactionSchedulerHandle, traits::LatestBlockProvider,
};
use magicblock_program::{
    magic_scheduled_base_intent::BaseActionCallback,
    validator::validator_authority,
};
use solana_instruction::{AccountMeta, Instruction};
use solana_signature::Signature;
use solana_signer::Signer;
use solana_transaction::Transaction;
use solana_transaction_error::TransactionError;

use crate::intent_executor::error::{
    IntentExecutorError, TransactionStrategyExecutionError,
};

pub trait ActionsCallbackExecutor: Send + Sync + Clone + 'static {
    /// Executes actions callbacks
    fn execute(&self, callbacks: Vec<BaseActionCallback>, result: ActionResult);
}

#[derive(Clone)]
pub struct ActionsCallbackExecutorImpl<L> {
    scheduler: TransactionSchedulerHandle,
    latest_block: L,
}

impl<L: LatestBlockProvider> ActionsCallbackExecutorImpl<L> {
    pub fn new(scheduler: TransactionSchedulerHandle, latest_block: L) -> Self {
        Self {
            scheduler,
            latest_block,
        }
    }

    fn instruction(
        callback: BaseActionCallback,
        authority: &solana_pubkey::Pubkey,
        succeeded: bool,
    ) -> Instruction {
        let mut data = callback.discriminator;
        data.push(succeeded as u8);
        data.extend(callback.payload);
        let account_metas =
            std::iter::once(AccountMeta::new_readonly(*authority, true))
                .chain(callback.account_metas_per_program.into_iter().map(
                    |m| {
                        if m.is_writable {
                            AccountMeta {
                                pubkey: m.pubkey,
                                // Can be writable only if not validator,
                                is_writable: &m.pubkey != authority,
                                is_signer: false,
                            }
                        } else {
                            AccountMeta::new_readonly(m.pubkey, false)
                        }
                    },
                ))
                .collect();
        Instruction::new_with_bytes(
            callback.destination_program,
            &data,
            account_metas,
        )
    }
}

impl<L: LatestBlockProvider> ActionsCallbackExecutor
    for ActionsCallbackExecutorImpl<L>
{
    fn execute(
        &self,
        callbacks: Vec<BaseActionCallback>,
        result: ActionResult,
    ) {
        let scheduler = self.scheduler.clone();
        let blockhash = self.latest_block.blockhash();
        tokio::spawn(async move {
            let authority = validator_authority();
            for callback in callbacks {
                let ix = Self::instruction(
                    callback,
                    &authority.pubkey(),
                    result.is_ok(),
                );
                let tx = Transaction::new_signed_with_payer(
                    &[ix],
                    Some(&authority.pubkey()),
                    &[&authority],
                    blockhash,
                );
                if let Err(err) = scheduler.schedule(tx).await {
                    tracing::error!(
                        error = ?err,
                        "Failed to schedule action callback"
                    );
                }
            }
        });
    }
}

#[derive(thiserror::Error, Debug, Clone)]
pub enum ActionError {
    #[error("Actions expired")]
    TimeoutError,
    #[error("User supplied actions are ill-formed: {0}. {:?}", .1)]
    ActionsError(#[source] TransactionError, Option<Signature>),
    #[error("Intent execution failed: {0}")]
    IntentFailedError(String),
}

pub type ActionResult = Result<(), ActionError>;

impl From<&TransactionStrategyExecutionError> for ActionError {
    fn from(value: &TransactionStrategyExecutionError) -> Self {
        if let TransactionStrategyExecutionError::ActionsError(err, signature) =
            value
        {
            Self::ActionsError(err.clone(), *signature)
        } else {
            Self::IntentFailedError(value.to_string())
        }
    }
}

impl From<&IntentExecutorError> for ActionError {
    fn from(value: &IntentExecutorError) -> Self {
        match value {
            IntentExecutorError::FailedToCommitError { err, .. }
            | IntentExecutorError::FailedToFinalizeError { err, .. } => {
                err.into()
            }
            err => ActionError::IntentFailedError(err.to_string()),
        }
    }
}
