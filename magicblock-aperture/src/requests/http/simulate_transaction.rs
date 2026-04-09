use std::collections::HashMap;

use magicblock_core::link::transactions::TransactionSimulationResult;
use solana_message::inner_instruction::InnerInstructions;
use solana_rpc_client_api::{
    config::RpcSimulateTransactionConfig,
    response::{RpcBlockhash, RpcSimulateTransactionResult},
};
use solana_transaction_status::{
    InnerInstruction, InnerInstructions as StatusInnerInstructions,
    UiTransactionEncoding,
};
use tracing::*;

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
                debug!(error = ?err, "Failed to prepare transaction to simulate")
            })?;
        self.ensure_transaction_accounts(&transaction.txn).await?;
        let number_of_accounts = transaction.txn.message().account_keys().len();

        let replacement_blockhash = config
            .replace_recent_blockhash
            .then(|| RpcBlockhash::from(self.blocks.get_latest()));
        let inner_instructions_enabled = config.inner_instructions;
        let accounts_config = config.accounts;

        // Submit the transaction to the scheduler for simulation.
        let result = self
            .transactions_scheduler
            .simulate(transaction.txn)
            .await
            .map_err(RpcError::transaction_simulation)?;
        let TransactionSimulationResult {
            result,
            logs,
            post_simulation_accounts,
            units_consumed,
            return_data,
            inner_instructions: recorded_inner_instructions,
        } = result;
        let accounts = if let Some(config_accounts) = accounts_config {
            let accounts_encoding = config_accounts
                .encoding
                .unwrap_or(UiAccountEncoding::Base64);

            if accounts_encoding == UiAccountEncoding::Binary
                || accounts_encoding == UiAccountEncoding::Base58
            {
                return Err(RpcError::invalid_params(
                    "base58 encoding not supported",
                ));
            }

            if config_accounts.addresses.len() > number_of_accounts {
                return Err(RpcError::invalid_params(format!(
                    "Too many accounts provided; max {number_of_accounts}"
                )));
            }

            if result.is_err() {
                Some(vec![None; config_accounts.addresses.len()])
            } else {
                let pubkeys = config_accounts
                    .addresses
                    .into_iter()
                    .map(|address| {
                        address
                            .parse::<Pubkey>()
                            .map_err(RpcError::invalid_params)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let current_accounts =
                    self.read_accounts_with_ensure(&pubkeys).await;
                let post_simulation_accounts = post_simulation_accounts
                    .into_iter()
                    .collect::<HashMap<_, _>>();

                Some(
                    pubkeys
                        .into_iter()
                        .zip(current_accounts)
                        .map(|(pubkey, account)| {
                            post_simulation_accounts
                                .get(&pubkey)
                                .cloned()
                                .or(account)
                                .map(|account| {
                                    LockedAccount::new(pubkey, account)
                                        .ui_encode(accounts_encoding, None)
                                })
                        })
                        .collect(),
                )
            }
        } else {
            None
        };

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

        let inner_instructions = inner_instructions_enabled.then(|| {
            recorded_inner_instructions
                .into_iter()
                .flatten()
                .enumerate()
                .map(converter)
                .collect::<Vec<_>>()
        });

        let result = RpcSimulateTransactionResult {
            logs,
            accounts,
            units_consumed: Some(units_consumed),
            return_data: return_data.map(Into::into),
            err: result.err().map(Into::into),
            loaded_accounts_data_size: None,
            inner_instructions,
            replacement_blockhash,
            fee: None,
            pre_balances: None,
            post_balances: None,
            pre_token_balances: None,
            post_token_balances: None,
            loaded_addresses: None,
        };

        let slot = self.blocks.block_height();
        Ok(ResponsePayload::encode(&request.id, result, slot))
    }
}
