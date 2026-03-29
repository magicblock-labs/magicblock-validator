use std::{collections::HashSet, sync::Arc};

use futures_util::future::join_all;
use magicblock_core::{
    intent::BaseActionCallback,
    traits::{
        ActionResult, ActionsCallbackScheduler, CallbackScheduleError,
        LatestBlockProvider,
    },
};
use magicblock_magic_program_api::{
    instruction::MagicBlockInstruction, MAGIC_CONTEXT_PUBKEY,
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
use tracing::{debug, error};

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

    fn build_instruction(
        callback: BaseActionCallback,
        authority: &Pubkey,
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
        let authority_pubkey = self.authority.pubkey();

        // Extract normalized writable accounts per callback BEFORE consuming them.
        // Applies the same authority exclusion as build_instruction (line 114):
        // validator authority is never writable in the callback instruction,
        // so it must not appear in the auto-commit target list either.
        let per_callback_accounts: Vec<Vec<Pubkey>> = callbacks
            .iter()
            .map(|cb| {
                cb.account_metas_per_program
                    .iter()
                    .filter(|m| m.is_writable && m.pubkey != authority_pubkey)
                    .map(|m| m.pubkey)
                    .collect()
            })
            .collect();

        let transactions_result =
            self.build_transactions(callbacks, signature, result);

        let mut valid_transactions = vec![];
        let mut valid_accounts = vec![];
        let signatures = transactions_result
            .into_iter()
            .enumerate()
            .map(|(i, el)| match el {
                Ok(tx) => {
                    let signature = *tx.get_signature();
                    valid_transactions.push(tx);
                    valid_accounts.push(per_callback_accounts[i].clone());
                    Ok(signature)
                }
                Err(err) => Err(err),
            })
            .collect();

        if !valid_transactions.is_empty() {
            let rpc_client = self.rpc_client.clone();
            let authority = self.authority.insecure_clone();
            let latest_block = self.latest_block.clone();

            tokio::spawn(async move {
                // 1. Send callback transactions and await confirmation
                let send_results = join_all(
                    valid_transactions
                        .iter()
                        .map(|tx| rpc_client.send_and_confirm_transaction(tx)),
                )
                .await;

                // 2. Collect writable accounts only from confirmed callbacks
                let mut confirmed_accounts = HashSet::new();
                for (i, result) in send_results.iter().enumerate() {
                    match result {
                        Ok(_) => {
                            if let Some(accounts) = valid_accounts.get(i) {
                                confirmed_accounts.extend(accounts);
                            }
                        }
                        Err(err) => {
                            error!(
                                error = ?err,
                                "Failed to send action callback transaction"
                            );
                        }
                    }
                }

                // 3. Auto-commit only accounts modified by confirmed callbacks.
                //    ScheduleCommit accepts validator authority as signer,
                //    bypassing the CPI ownership check.
                if !confirmed_accounts.is_empty() {
                    let accounts: Vec<Pubkey> =
                        confirmed_accounts.into_iter().collect();
                    if let Err(err) = Self::schedule_auto_commit(
                        &rpc_client,
                        &authority,
                        &latest_block,
                        &accounts,
                    )
                    .await
                    {
                        error!(
                            error = ?err,
                            "Failed to auto-commit post-callback state"
                        );
                    }
                }
            });
        }

        signatures
    }
}

impl<L: LatestBlockProvider> ActionsCallbackService<L> {
    /// Schedule a standalone commit for accounts modified by a callback.
    ///
    /// After a callback transaction confirms on ER, the modified account state
    /// exists only in ER memory. This method sends a `ScheduleCommit`
    /// transaction so that the slot ticker picks it up via
    /// `AcceptScheduleCommits` and the committor service flushes the updated
    /// state to L1 within the same or next ER slot.
    async fn schedule_auto_commit(
        rpc_client: &RpcClient,
        authority: &Keypair,
        latest_block: &L,
        accounts: &[Pubkey],
    ) -> Result<Signature, Box<dyn std::error::Error + Send + Sync>> {
        let blockhash = latest_block.blockhash();
        let authority_pubkey = authority.pubkey();

        let mut account_metas = Vec::with_capacity(2 + accounts.len());
        account_metas
            .push(AccountMeta::new(authority_pubkey, true));
        account_metas
            .push(AccountMeta::new(MAGIC_CONTEXT_PUBKEY, false));
        for pubkey in accounts {
            account_metas.push(AccountMeta::new_readonly(*pubkey, false));
        }

        let ix = Instruction::new_with_bincode(
            magicblock_magic_program_api::id(),
            &MagicBlockInstruction::ScheduleCommit,
            account_metas,
        );
        let message = Message::new_with_blockhash(
            &[ix],
            Some(&authority_pubkey),
            &blockhash,
        );
        let tx = VersionedTransaction::try_new(
            VersionedMessage::Legacy(message),
            &[authority],
        )?;

        let sig = rpc_client.send_and_confirm_transaction(&tx).await?;
        debug!(
            signature = %sig,
            accounts = accounts.len(),
            "Post-callback auto-commit scheduled"
        );
        Ok(sig)
    }
}
