use std::sync::Arc;

use futures_util::future::join_all;
use magicblock_core::{
    intent::BaseActionCallback,
    traits::{
        ActionResult, ActionsCallbackScheduler, CallbackScheduleError,
        LatestBlockProvider,
    },
};
use magicblock_magic_program_api::response::{ActionReceipt, MagicResponse};
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_message::{Message, VersionedMessage};
use solana_pubkey::Pubkey;
use solana_rpc_client::{
    nonblocking::rpc_client::RpcClient, rpc_client::SerializableTransaction,
};
use solana_signature::Signature;
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;
use tracing::error;

pub struct ActionsCallbackService<L> {
    rpc_client: Arc<RpcClient>,
    authority: Keypair,
    latest_block: L,
}

impl<L: Clone> Clone for ActionsCallbackService<L> {
    fn clone(&self) -> Self {
        Self {
            rpc_client: self.rpc_client.clone(),
            authority: self.authority.insecure_clone(),
            latest_block: self.latest_block.clone(),
        }
    }
}

impl<L: LatestBlockProvider> ActionsCallbackService<L> {
    pub fn new(
        rpc_client: Arc<RpcClient>,
        authority: Keypair,
        latest_block: L,
    ) -> Self {
        Self {
            rpc_client,
            authority,
            latest_block,
        }
    }

    fn build_transactions(
        &self,
        callbacks: Vec<BaseActionCallback>,
        signature: Option<Signature>,
        result: ActionResult,
    ) -> Vec<Result<VersionedTransaction, CallbackScheduleError>> {
        let authority_pubkey = self.authority.pubkey();
        let blockhash = self.latest_block.blockhash();
        let result: Result<(), String> = result.map_err(|e| e.to_string());

        callbacks
            .into_iter()
            .map(|callback| {
                let ix = Self::build_instruction(
                    callback,
                    &authority_pubkey,
                    signature,
                    result.clone(),
                )?;
                let message = Message::new_with_blockhash(
                    &[ix],
                    Some(&authority_pubkey),
                    &blockhash,
                );
                VersionedTransaction::try_new(
                    VersionedMessage::Legacy(message),
                    &[&self.authority],
                )
                .map_err(|e| CallbackScheduleError::SigningError(e.to_string()))
            })
            .collect()
    }

    fn build_action_receipt(signature: Signature) -> ActionReceipt {
        ActionReceipt {
            signature,
            slot: None,
            logs: None,
        }
    }

    fn build_instruction(
        callback: BaseActionCallback,
        authority: &Pubkey,
        signature: Option<Signature>,
        result: Result<(), String>,
    ) -> Result<Instruction, CallbackScheduleError> {
        let response = MagicResponse {
            ok: result.is_ok(),
            data: callback.payload,
            error: result.err().unwrap_or_default(),
            receipt: signature.map(Self::build_action_receipt),
        };
        let mut data = callback.discriminator;
        data.extend(
            bincode::serialize(&response)
                .map_err(CallbackScheduleError::SerializationError)?,
        );
        let account_metas =
            std::iter::once(AccountMeta::new_readonly(*authority, true))
                .chain(callback.account_metas_per_program.into_iter().map(
                    |m| {
                        if m.is_writable {
                            AccountMeta {
                                pubkey: m.pubkey,
                                // Can be writable only if not the validator
                                is_writable: &m.pubkey != authority,
                                is_signer: false,
                            }
                        } else {
                            AccountMeta::new_readonly(m.pubkey, false)
                        }
                    },
                ))
                .collect();
        Ok(Instruction::new_with_bytes(
            callback.destination_program,
            &data,
            account_metas,
        ))
    }
}

impl<L: LatestBlockProvider> ActionsCallbackScheduler
    for ActionsCallbackService<L>
{
    fn schedule(
        &self,
        callbacks: Vec<BaseActionCallback>,
        signature: Option<Signature>,
        result: ActionResult,
    ) -> Vec<Result<Signature, CallbackScheduleError>> {
        let transactions_result =
            self.build_transactions(callbacks, signature, result);

        let mut valid_transactions = vec![];
        let signatures = transactions_result
            .into_iter()
            .map(|el| match el {
                Ok(tx) => {
                    let signature = *tx.get_signature();
                    valid_transactions.push(tx);
                    Ok(signature)
                }
                Err(err) => Err(err),
            })
            .collect();

        if !valid_transactions.is_empty() {
            let rpc_client = self.rpc_client.clone();
            tokio::spawn(async move {
                let send_futs = valid_transactions
                    .iter()
                    .map(|tx| rpc_client.send_transaction(tx));
                join_all(send_futs).await.into_iter().for_each(|result| {
                    if let Err(err) = result {
                        error!(
                            error = ?err,
                            "Failed to send action callback transaction"
                        );
                    }
                });
            });
        }

        signatures
    }
}
