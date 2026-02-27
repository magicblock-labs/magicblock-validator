use magicblock_core::{
    link::transactions::TransactionSchedulerHandle, traits::LatestBlockProvider,
};
use magicblock_program::{
    magic_scheduled_base_intent::BaseActionCallback,
    validator::validator_authority,
};
use solana_instruction::{AccountMeta, Instruction};
use solana_signer::Signer;

use crate::intent_executor::ActionsCallbackExecutor;

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
    fn execute(&self, callbacks: Vec<BaseActionCallback>, succeeded: bool) {
        let scheduler = self.scheduler.clone();
        let blockhash = self.latest_block.blockhash();
        tokio::spawn(async move {
            let authority = validator_authority();
            for callback in callbacks {
                let ix =
                    Self::instruction(callback, &authority.pubkey(), succeeded);
                let tx = solana_transaction::Transaction::new_signed_with_payer(
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
