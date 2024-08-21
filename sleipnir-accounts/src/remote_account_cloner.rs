use std::sync::Arc;

use async_trait::async_trait;
use sleipnir_bank::bank::Bank;
use sleipnir_mutator::{
    transactions::transactions_to_clone_account_from_cluster, Cluster,
};
use sleipnir_program::sleipnir_instruction::AccountModification;
use sleipnir_transaction_status::TransactionStatusSender;
use solana_sdk::{account::Account, pubkey::Pubkey, signature::Signature};

use crate::{
    errors::AccountsResult, utils::execute_legacy_transaction, AccountCloner,
};

pub struct RemoteAccountCloner {
    cluster: Cluster,
    bank: Arc<Bank>,
    transaction_status_sender: Option<TransactionStatusSender>,
}

impl RemoteAccountCloner {
    pub fn new(
        cluster: Cluster,
        bank: Arc<Bank>,
        transaction_status_sender: Option<TransactionStatusSender>,
    ) -> Self {
        Self {
            cluster,
            bank,
            transaction_status_sender,
        }
    }
}

#[async_trait]
impl AccountCloner for RemoteAccountCloner {
    async fn clone_account(
        &self,
        pubkey: &Pubkey,
        account: Option<Account>,
        overrides: Option<AccountModification>,
    ) -> AccountsResult<Vec<Signature>> {
        let slot = self.bank.slot();
        let blockhash = self.bank.last_blockhash();
        let clone_txs = transactions_to_clone_account_from_cluster(
            &self.cluster,
            &pubkey,
            account,
            blockhash,
            slot,
            overrides,
        )
        .await?;

        let signatures = clone_txs
            .into_iter()
            .map(|clone_tx| {
                execute_legacy_transaction(
                    clone_tx,
                    &self.bank,
                    self.transaction_status_sender.as_ref(),
                )
            })
            .collect::<Result<_, _>>()?;

        Ok(signatures)
    }
}
