use async_trait::async_trait;
use dlp::instruction::{commit_state, finalize};
use solana_sdk::{
    account::{AccountSharedData, ReadableAccount},
    pubkey::Pubkey,
    signature::{Keypair, Signature},
    signer::Signer,
    system_program,
    transaction::Transaction,
};

use crate::{
    errors::{AccountsError, AccountsResult},
    AccountCommitter,
};
use solana_rpc_client::nonblocking::rpc_client::RpcClient;

pub struct RemoteAccountCommitter {
    rpc_client: RpcClient,
    committer_authority: Keypair,
}

impl RemoteAccountCommitter {
    pub fn new(rpc_client: RpcClient, committer_authority: Keypair) -> Self {
        Self {
            rpc_client,
            committer_authority,
        }
    }
}

#[async_trait]
impl AccountCommitter for RemoteAccountCommitter {
    async fn commit_account(
        &self,
        delegated_account: Pubkey,
        committed_state_data: AccountSharedData,
    ) -> AccountsResult<Signature> {
        let committer = self.committer_authority.pubkey();
        let commit_ix = commit_state(
            committer,
            delegated_account,
            system_program::id(),
            committed_state_data.data().to_vec(),
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
            &[commit_ix, finalize_ix],
            Some(&self.committer_authority.pubkey()),
            &[&self.committer_authority],
            latest_blockhash,
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

        Ok(signature)
    }
}
