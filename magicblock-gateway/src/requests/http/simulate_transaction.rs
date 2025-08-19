use solana_message::{
    inner_instruction::InnerInstructions, SimpleAddressLoader,
};
use solana_rpc_client_api::{
    config::RpcSimulateTransactionConfig,
    response::{RpcBlockhash, RpcSimulateTransactionResult},
};
use solana_transaction::{
    sanitized::SanitizedTransaction,
    versioned::sanitized::SanitizedVersionedTransaction,
};
use solana_transaction_status::{
    InnerInstruction, InnerInstructions as StatusInnerInstructions,
    UiTransactionEncoding,
};

use super::prelude::*;

impl HttpDispatcher {
    pub(crate) async fn simulate_transaction(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
        let (transaction, config) = parse_params!(
            request.params()?,
            String,
            RpcSimulateTransactionConfig
        );
        let transaction = transaction.ok_or_else(|| {
            RpcError::invalid_params("missing encoded transaction")
        })?;
        let config = config.unwrap_or_default();
        let encoding = config.encoding.unwrap_or(UiTransactionEncoding::Base58);
        let transaction = self.decode_transaction(&transaction, encoding)?;
        let (hash, replacement) = if config.replace_recent_blockhash {
            let latest = self.blocks.get_latest();
            (latest.hash, Some(RpcBlockhash::from(latest)))
        } else {
            (transaction.message.hash(), None)
        };
        let transaction = SanitizedVersionedTransaction::try_new(transaction)
            .map_err(RpcError::invalid_params)?;
        let transaction = SanitizedTransaction::try_new(
            transaction,
            hash,
            false,
            SimpleAddressLoader::Disabled,
            &Default::default(),
        )
        .map_err(RpcError::invalid_params)?;
        if config.sig_verify {
            transaction
                .verify()
                .map_err(RpcError::transaction_verification)?;
        }
        let message = transaction.message();
        let reader = self.accountsdb.reader().map_err(RpcError::internal)?;
        let mut ensured = false;
        loop {
            let mut to_ensure = Vec::new();
            let accounts = message.account_keys().iter().enumerate();
            for (index, pubkey) in accounts {
                if message.is_writable(index) {
                    match reader.read(pubkey, |account| account.delegated()) {
                        Some(true) => (),
                        Some(false) => {
                            Err(RpcError::invalid_params(
                                    "tried to use non-delegated account as writeable",
                            ))?;
                        }
                        None => to_ensure.push(*pubkey),
                    }
                } else if !reader.contains(pubkey) {
                    to_ensure.push(*pubkey);
                }
            }
            if ensured && !to_ensure.is_empty() {
                Err(RpcError::invalid_params(format!(
                    "transaction uses non-existent accounts: {to_ensure:?}"
                )))?;
            }
            if to_ensure.is_empty() {
                break;
            }
            let to_ensure = AccountsToEnsure::new(to_ensure);
            let ready = to_ensure.ready.clone();
            let _ = self.ensure_accounts_tx.send(to_ensure).await;
            ready.notified().await;

            ensured = true;
        }

        let result = self.transactions_scheduler.simulate(transaction).await?;
        let slot = self.accountsdb.slot();
        let converter = |(index, ixs): (usize, InnerInstructions)| {
            StatusInnerInstructions {
                index: index as u8,
                instructions: ixs
                    .into_iter()
                    .map(|ix| InnerInstruction {
                        instruction: ix.instruction,
                        stack_height: Some(ix.stack_height as u32),
                    })
                    .collect(),
            }
            .into()
        };
        let result = RpcSimulateTransactionResult {
            err: result.result.err(),
            logs: result.logs,
            accounts: None,
            units_consumed: Some(result.units_consumed),
            return_data: result.return_data.map(Into::into),
            inner_instructions: result
                .inner_instructions
                .into_iter()
                .flatten()
                .enumerate()
                .map(converter)
                .collect::<Vec<_>>()
                .into(),
            replacement_blockhash: replacement,
        };
        Ok(ResponsePayload::encode(&request.id, result, slot))
    }
}
