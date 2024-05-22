use async_trait::async_trait;
use sleipnir_processor::batch_processor::{
    execute_batch, TransactionBatchWithIndexes,
};
use solana_sdk::{
    pubkey::Pubkey, signature::Signature, transaction::SanitizedTransaction,
};
use std::sync::Arc;

use sleipnir_bank::bank::Bank;
use sleipnir_mutator::{
    mutator::transaction_to_clone_account_from_cluster, Cluster,
};

use crate::{errors::AccountsResult, AccountCloner};

pub struct RemoteAccountCloner {
    cluster: Cluster,
    bank: Arc<Bank>,
}

impl RemoteAccountCloner {
    pub fn new(cluster: Cluster, bank: Arc<Bank>) -> Self {
        Self { cluster, bank }
    }
}

#[async_trait]
impl AccountCloner for RemoteAccountCloner {
    async fn clone_account(
        &self,
        pubkey: &Pubkey,
    ) -> AccountsResult<Signature> {
        let slot = self.bank.slot();
        let blockhash = self.bank.last_blockhash();
        let clone_tx = transaction_to_clone_account_from_cluster(
            &self.cluster,
            &pubkey.to_string(),
            blockhash,
            slot,
        )
        .await?;
        let sanitized_tx =
            SanitizedTransaction::try_from_legacy_transaction(clone_tx)?;
        let signature = *sanitized_tx.signature();
        let txs = &[sanitized_tx];
        let batch = self.bank.prepare_sanitized_batch(txs);

        let batch_with_indexes = TransactionBatchWithIndexes {
            batch,
            transaction_indexes: txs
                .iter()
                .enumerate()
                .map(|(idx, _)| idx)
                .collect(),
        };
        let mut timings = Default::default();
        execute_batch(
            &batch_with_indexes,
            &self.bank,
            None,
            &mut timings,
            None,
        )?;

        Ok(signature)
    }
}
