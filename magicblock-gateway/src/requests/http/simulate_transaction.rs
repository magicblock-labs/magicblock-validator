use solana_message::inner_instruction::InnerInstructions;
use solana_rpc_client_api::{
    config::RpcSimulateTransactionConfig,
    response::RpcSimulateTransactionResult,
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
        let transaction = self.prepare_transaction(
            &transaction,
            encoding,
            config.sig_verify,
            config.replace_recent_blockhash,
        )?;
        self.ensure_transaction_accounts(&transaction).await?;
        let slot = self.accountsdb.slot();

        let replacement = config
            .replace_recent_blockhash
            .then(|| self.blocks.get_latest().into());

        let result = self.transactions_scheduler.simulate(transaction).await?;

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
