use std::{collections::HashMap, sync::RwLock};

use async_trait::async_trait;
use dlp::instruction::{commit_state, finalize};
use log::*;
use solana_rpc_client::{
    nonblocking::rpc_client::RpcClient, rpc_client::SerializableTransaction,
};
use solana_rpc_client_api::config::RpcSendTransactionConfig;
use solana_sdk::{
    account::{AccountSharedData, ReadableAccount},
    commitment_config::CommitmentConfig,
    compute_budget::ComputeBudgetInstruction,
    instruction::Instruction,
    pubkey::Pubkey,
    signature::{Keypair, Signature},
    signer::Signer,
    transaction::Transaction,
};

use crate::{
    errors::{AccountsError, AccountsResult},
    AccountCommitter,
};

pub struct RemoteAccountCommitter {
    rpc_client: RpcClient,
    committer_authority: Keypair,
    /// Tracking the last commit we did for each pubkey.
    /// This increases memory usage, but allows us to check this without
    /// downloading the currently committed account data from chain.
    commits: RwLock<HashMap<Pubkey, AccountSharedData>>,
    compute_unit_price: u64,
}

impl RemoteAccountCommitter {
    pub fn new(
        rpc_client: RpcClient,
        committer_authority: Keypair,
        compute_unit_price: u64,
    ) -> Self {
        Self {
            rpc_client,
            committer_authority,
            commits: RwLock::<HashMap<Pubkey, AccountSharedData>>::default(),
            compute_unit_price,
        }
    }
}

#[async_trait]
impl AccountCommitter for RemoteAccountCommitter {
    async fn create_commit_account_transaction(
        &self,
        delegated_account: Pubkey,
        commit_state_data: AccountSharedData,
    ) -> AccountsResult<Option<Transaction>> {
        if let Some(committed_account) = self
            .commits
            .read()
            .expect("RwLock commits poisoned")
            .get(&delegated_account)
        {
            if committed_account.data() == commit_state_data.data() {
                return Ok(None);
            }
        }
        let (compute_budget_ix, compute_unit_price_ix) =
            self.compute_instructions();

        let committer = self.committer_authority.pubkey();
        let commit_ix = commit_state(
            committer,
            delegated_account,
            commit_state_data.data().to_vec(),
        );
        let finalize_ix = finalize(committer, delegated_account, committer);
        // NOTE: this is an async request that the transaction thread waits for to
        // be completed. It's not ideal, but the only way to create the transaction
        // and obtain its signature to be logged for the trigger commit.
        let latest_blockhash = self
            .rpc_client
            .get_latest_blockhash()
            .await
            .map_err(|err| {
                AccountsError::FailedToGetLatestBlockhash(err.to_string())
            })?;

        let tx = Transaction::new_signed_with_payer(
            &[
                compute_budget_ix,
                compute_unit_price_ix,
                commit_ix,
                finalize_ix,
            ],
            Some(&self.committer_authority.pubkey()),
            &[&self.committer_authority],
            latest_blockhash,
        );
        Ok(Some(tx))
    }

    async fn commit_account(
        &self,
        delegated_account: Pubkey,
        commit_state_data: AccountSharedData,
        transaction: Transaction,
    ) -> AccountsResult<Signature> {
        let tx_sig = transaction.get_signature();
        debug!(
            "Committing account '{}' sig: {:?} to {}",
            delegated_account,
            tx_sig,
            self.rpc_client.url()
        );
        let signature = self
            .rpc_client
            .send_transaction_with_config(
                &transaction,
                RpcSendTransactionConfig {
                    skip_preflight: true,
                    ..Default::default()
                },
            )
            .await
            .map_err(|err| {
                AccountsError::FailedToSendTransaction(err.to_string())
            })?;

        if &signature != tx_sig {
            error!(
                "Transaction Signature mismatch: {:?} != {:?}",
                signature, tx_sig
            );
        }
        debug!(
            "Sent commit for '{}' | signature: '{:?}'",
            delegated_account, signature
        );

        self.rpc_client
            .confirm_transaction_with_commitment(
                &signature,
                CommitmentConfig::confirmed(),
            )
            .await
            .map_err(|err| {
                AccountsError::FailedToConfirmTransaction(err.to_string())
            })?;

        debug!(
            "Confirmed commit for '{}' | signature: '{:?}'",
            delegated_account, signature
        );

        self.commits
            .write()
            .expect("RwLock commits poisoned")
            .insert(delegated_account, commit_state_data);

        Ok(signature)
    }
}

impl RemoteAccountCommitter {
    fn compute_instructions(&self) -> (Instruction, Instruction) {
        // TODO(thlorenz): We may need to compute this budget from the account size since
        // the account is copied which could affect CUs
        const COMPUTE_BUDGET: u32 = 80_000;

        let compute_budget_ix =
            ComputeBudgetInstruction::set_compute_unit_limit(COMPUTE_BUDGET);
        let compute_unit_price_ix =
            ComputeBudgetInstruction::set_compute_unit_price(
                self.compute_unit_price,
            );
        (compute_budget_ix, compute_unit_price_ix)
    }
}
