use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use engine::Engine;
use futures_util::future::join_all;
use magicblock_core::{
    intent::BaseActionCallback,
    traits::{ActionResult, ActionsCallbackScheduler, CallbackScheduleError},
};
use magicblock_magic_program_api::{
    CALLBACK_PROGRAM_ID,
    instruction::{CallbackInstruction, MagicBlockInstruction},
    pda::CALLBACK_SIGNER,
    response::{ActionReceipt, MagicResponse, MagicResponseV1},
};
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
use tracing::info;

pub struct ActionsCallbackService {
    rpc_client: Arc<RpcClient>,
    authority: Keypair,
    engine: Engine,
}

impl Clone for ActionsCallbackService {
    fn clone(&self) -> Self {
        Self {
            rpc_client: self.rpc_client.clone(),
            authority: self.authority.insecure_clone(),
            engine: self.engine.clone(),
        }
    }
}

impl ActionsCallbackService {
    pub fn new(
        rpc_client: Arc<RpcClient>,
        authority: Keypair,
        engine: Engine,
    ) -> Self {
        Self {
            rpc_client,
            authority,
            engine,
        }
    }

    fn build_transactions(
        &self,
        callbacks: Vec<BaseActionCallback>,
        signature: Option<Signature>,
        result: ActionResult,
    ) -> Vec<Result<VersionedTransaction, CallbackScheduleError>> {
        /// Counter used to make each callback transaction unique
        static TX_COUNTER: AtomicU64 = AtomicU64::new(0);

        let authority_pubkey = self.authority.pubkey();
        let blockhash = self.engine.blockhash();
        let result: Result<(), String> = result.map_err(|e| e.to_string());

        callbacks
            .into_iter()
            .map(|callback| {
                // Make each callback TX unique
                let counter = TX_COUNTER.fetch_add(1, Ordering::Relaxed);
                let noop_ix = Instruction::new_with_wincode(
                    magicblock_magic_program_api::ID,
                    &MagicBlockInstruction::Noop(counter),
                    vec![],
                );

                let callback_ix = Self::build_callback_instruction(
                    callback,
                    &authority_pubkey,
                    signature,
                    result.clone(),
                )?;
                let message = Message::new_with_blockhash(
                    &[noop_ix, callback_ix],
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

    fn build_callback_instruction(
        callback: BaseActionCallback,
        authority: &Pubkey,
        signature: Option<Signature>,
        result: Result<(), String>,
    ) -> Result<Instruction, CallbackScheduleError> {
        let inner_instruction =
            Self::build_inner_instruction(callback, signature, result)?;

        // Mandatory accounts for magic-program
        let mut account_metas = vec![
            AccountMeta::new_readonly(*authority, true),
            AccountMeta::new_readonly(CALLBACK_SIGNER, false),
            AccountMeta::new_readonly(inner_instruction.program_id, false),
        ];
        account_metas.extend(
            inner_instruction
                .accounts
                .clone()
                .into_iter()
                .map(|mut el| {
                    // CALLBACK_SIGNER may be set to true in inner_instruction
                    // Outer instruction can't have PDA as signer
                    el.is_signer = false;
                    el
                }),
        );

        let data = CallbackInstruction::ExecuteCallback {
            instruction: inner_instruction,
        };
        let instruction = Instruction::new_with_wincode(
            CALLBACK_PROGRAM_ID,
            &data,
            account_metas,
        );

        Ok(instruction)
    }

    fn build_inner_instruction(
        callback: BaseActionCallback,
        signature: Option<Signature>,
        result: Result<(), String>,
    ) -> Result<Instruction, CallbackScheduleError> {
        let response = MagicResponse::V1(MagicResponseV1 {
            ok: result.is_ok(),
            data: callback.payload,
            error: result.err().unwrap_or_default(),
            receipt: signature.map(|signature| ActionReceipt { signature }),
        });
        let mut data = callback.discriminator;
        data.extend(
            wincode::serialize(&response)
                .map_err(CallbackScheduleError::SerializationError)?,
        );

        let account_metas = callback
            .account_metas_per_program
            .into_iter()
            .map(|m| {
                // account invariants checked within magic-program & callback-prpgram
                // here we just propagate
                let is_signer = m.pubkey == CALLBACK_SIGNER;
                AccountMeta {
                    pubkey: m.pubkey,
                    is_writable: m.is_writable,
                    is_signer,
                }
            })
            .collect();
        Ok(Instruction::new_with_bytes(
            callback.destination_program,
            &data,
            account_metas,
        ))
    }
}

impl ActionsCallbackScheduler for ActionsCallbackService {
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
                join_all(send_futs).await.into_iter().enumerate().for_each(
                    |(i, result)| {
                        if let Err(err) = result {
                            let signature =
                                valid_transactions[i].get_signature();
                            info!(
                                error = ?err,
                                signature = ?signature,
                                "Failed to send action callback transaction"
                            );
                        }
                    },
                );
            });
        }

        signatures
    }
}
