use async_trait::async_trait;
use solana_sdk::{
    account::AccountSharedData,
    pubkey::Pubkey,
    signature::{Keypair, Signature},
    signer::Signer,
};

use crate::{errors::AccountsResult, AccountCommitter};

pub struct RemoteAccountCommitter {
    committer_authority: Keypair,
}

impl RemoteAccountCommitter {
    pub fn new(committer_authority: Keypair) -> Self {
        Self {
            committer_authority,
        }
    }
}

#[async_trait]
impl AccountCommitter for RemoteAccountCommitter {
    async fn commit_account(
        &self,
        _delegated_account: Pubkey,
        _committed_state_data: AccountSharedData,
    ) -> AccountsResult<Signature> {
        todo!("commit account with {}", self.committer_authority.pubkey())
    }
}
