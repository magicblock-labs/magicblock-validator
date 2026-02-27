use magicblock_core::link::transactions::TransactionSchedulerHandle;
use magicblock_program::{
    magic_scheduled_base_intent::BaseActionCallback,
    validator::validator_authority,
};
use solana_signer::Signer;

use crate::intent_executor::ActionsCallbackExecutor;

#[derive(Clone)]
pub struct ActionsCallbackExecutorImpl {
    scheduler: TransactionSchedulerHandle,
}

impl ActionsCallbackExecutorImpl {
    pub fn new(scheduler: TransactionSchedulerHandle) -> Self {
        Self { scheduler }
    }
}

impl ActionsCallbackExecutor for ActionsCallbackExecutorImpl {
    fn execute(&self, callbacks: Vec<BaseActionCallback>, succeeded: bool) {
        let scheduler = self.scheduler.clone();
        tokio::spawn(async move {
            for callback in callbacks {
                let authority = validator_authority();
                let mut data = callback.discriminator;
                data.push(succeeded as u8);
                data.extend(callback.payload);
                let account_metas = callback
                    .account_metas_per_program
                    .into_iter()
                    .map(|m| {
                        if m.is_writable {
                            solana_instruction::AccountMeta::new(
                                m.pubkey, false,
                            )
                        } else {
                            solana_instruction::AccountMeta::new_readonly(
                                m.pubkey, false,
                            )
                        }
                    })
                    .collect::<Vec<_>>();
                let ix = solana_instruction::Instruction::new_with_bytes(
                    callback.destination_program,
                    &data,
                    account_metas,
                );
                let tx =
                    solana_transaction::Transaction::new_signed_with_payer(
                        &[ix],
                        Some(&authority.pubkey()),
                        &[&authority],
                        solana_hash::Hash::default(),
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
