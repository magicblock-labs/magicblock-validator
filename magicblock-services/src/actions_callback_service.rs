use std::sync::Arc;

use magicblock_core::{
    intent::BaseActionCallback,
    traits::{ActionResult, ActionsCallbackScheduler, LatestBlockProvider},
};
use magicblock_magic_program_api::response::MagicResponse;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_message::{Message, VersionedMessage};
use solana_pubkey::Pubkey;
use solana_rpc_client::{
    nonblocking::rpc_client::RpcClient, rpc_client::SerializableTransaction,
};
use solana_signature::Signature;
use solana_signer::{Signer, SignerError};
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
        result: ActionResult,
    ) -> Vec<Result<VersionedTransaction, SignerError>> {
        let authority_pubkey = self.authority.pubkey();
        let blockhash = self.latest_block.blockhash();
        let result: Result<(), String> = result.map_err(|e| e.to_string());

        callbacks
            .into_iter()
            .map(|callback| {
                let ix = Self::build_instruction(
                    callback,
                    &authority_pubkey,
                    result.clone(),
                );
                let message = Message::new_with_blockhash(
                    &[ix],
                    Some(&authority_pubkey),
                    &blockhash,
                );
                VersionedTransaction::try_new(
                    VersionedMessage::Legacy(message),
                    &[&self.authority],
                )
            })
            .collect()
    }

    fn build_instruction(
        callback: BaseActionCallback,
        authority: &Pubkey,
        result: Result<(), String>,
    ) -> Instruction {
        let response = MagicResponse {
            ok: result.is_ok(),
            data: callback.payload,
            error: result.err().unwrap_or_default(),
        };
        let mut data = callback.discriminator;
        data.extend(
            bincode::serialize(&response)
                .expect("MagicResponse serialization is infallible"),
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
        Instruction::new_with_bytes(
            callback.destination_program,
            &data,
            account_metas,
        )
    }
}

impl<L: LatestBlockProvider> ActionsCallbackScheduler
    for ActionsCallbackService<L>
{
    type ScheduleError = SignerError;

    fn schedule(
        &self,
        callbacks: Vec<BaseActionCallback>,
        result: ActionResult,
    ) -> Vec<Result<Signature, SignerError>> {
        let transactions_result = self.build_transactions(callbacks, result);

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
                for tx in valid_transactions {
                    if let Err(err) = rpc_client.send_transaction(&tx).await {
                        error!(
                            error = ?err,
                            "Failed to send action callback transaction"
                        );
                    }
                }
            });
        }

        signatures
    }
}
