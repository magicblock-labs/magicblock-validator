use log::*;
use solana_message::inner_instruction::InnerInstructions;
use solana_rpc_client_api::{
    config::RpcSimulateTransactionConfig,
    response::{RpcBlockhash, RpcSimulateTransactionResult},
};
use solana_transaction_status::{
    InnerInstruction, InnerInstructions as StatusInnerInstructions,
    UiTransactionEncoding,
};

use super::prelude::*;

impl HttpDispatcher {
    /// Handles the `simulateTransaction` RPC request.
    ///
    /// Simulates a transaction against the current state of the ledger without
    /// committing any changes. This is used for preflight checks. The simulation
    /// can be customized to skip signature verification or replace the transaction's
    /// blockhash with a recent one. Returns a detailed result including execution
    /// logs, compute units, and the simulation outcome.
    pub(crate) async fn simulate_transaction(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
        let (transaction_str, config) = parse_params!(
            request.params()?,
            String,
            RpcSimulateTransactionConfig
        );
        let transaction_str: String = some_or_err!(transaction_str);
        let config = config.unwrap_or_default();
        let encoding = config.encoding.unwrap_or(UiTransactionEncoding::Base58);

        // Prepare the transaction, applying simulation-specific options.
        let transaction = self
            .prepare_transaction(
                &transaction_str,
                encoding,
                config.sig_verify,
                config.replace_recent_blockhash,
            )
            .inspect_err(|err| {
                debug!("Failed to prepare transaction to simulate: {err}")
            })?;
        self.ensure_transaction_accounts(&transaction).await?;

        let replacement_blockhash = config
            .replace_recent_blockhash
            .then(|| RpcBlockhash::from(self.blocks.get_latest()));

        // Submit the transaction to the scheduler for simulation.
        let result = self
            .transactions_scheduler
            .simulate(transaction)
            .await
            .map_err(RpcError::transaction_simulation)?;

        // Convert the internal simulation result to the client-facing RPC format.
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
            replacement_blockhash,
        };

        let slot = self.blocks.block_height();
        Ok(ResponsePayload::encode(&request.id, result, slot))
    }
}
