use log::*;
use std::{collections::HashMap, sync::RwLock};

use async_trait::async_trait;
use dlp::instruction::{commit_state, finalize};
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    account::{AccountSharedData, ReadableAccount},
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
    async fn commit_account(
        &self,
        delegated_account: Pubkey,
        commit_state_data: AccountSharedData,
    ) -> AccountsResult<Option<Signature>> {
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

        debug!(
            "Sending commit transaction for account {}",
            delegated_account
        );
        let signature = self
            .rpc_client
            .send_and_confirm_transaction(&tx)
            .await
            .map_err(|err| {
                AccountsError::FailedToSendAndConfirmTransaction(
                    err.to_string(),
                )
            })?;
        debug!(
            "Confirmed commit for '{}' | signature: '{:?}'",
            delegated_account, signature
        );

        self.commits
            .write()
            .expect("RwLock commits poisoned")
            .insert(delegated_account, commit_state_data);

        Ok(Some(signature))
    }
}

impl RemoteAccountCommitter {
    fn compute_instructions(&self) -> (Instruction, Instruction) {
        // TODO(thlorenz): We may need to compute this budget from the account size since
        // the account is de/serialzalized which could affect CUs
        const COMPUTE_BUDGET: u32 = 20_000;

        let compute_budget_ix =
            ComputeBudgetInstruction::set_compute_unit_limit(COMPUTE_BUDGET);
        let compute_unit_price_ix =
            ComputeBudgetInstruction::set_compute_unit_price(
                self.compute_unit_price,
            );
        (compute_budget_ix, compute_unit_price_ix)
    }
}
