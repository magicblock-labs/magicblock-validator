use std::collections::HashMap;

use magicblock_metrics::metrics::AccountFetchContext;
use solana_account::AccountSharedData;
use solana_account_decoder::{UiAccountEncoding, encode_ui_account};
use solana_message::inner_instruction::InnerInstructions;
use solana_pubkey::Pubkey;
use solana_rpc_client_api::{
    config::RpcSimulateTransactionConfig,
    response::RpcSimulateTransactionResult,
};
use solana_svm::transaction_processing_result::TransactionProcessingResultExtensions;
use solana_transaction_status::{
    InnerInstruction, InnerInstructions as StatusInnerInstructions,
    UiTransactionEncoding,
};

use super::{
    ClaimedHandlerResult, HandlerResult, send_transaction::TransactionKind,
};
use crate::{
    error::RpcError,
    requests::{JsonHttpRequest as JsonRequest, payload::ResponsePayload},
    server::http::dispatch::HttpDispatcher,
};

impl HttpDispatcher {
    pub(crate) async fn simulate_transaction(
        &self,
        request: &JsonRequest,
    ) -> ClaimedHandlerResult {
        let mut claims = 0;
        let result =
            self.simulate_transaction_inner(request, &mut claims).await;
        (result, claims)
    }

    async fn simulate_transaction_inner(
        &self,
        request: &JsonRequest,
        claims: &mut u64,
    ) -> HandlerResult {
        let transaction_str = request.required::<String>(0)?;
        let config = request
            .optional::<RpcSimulateTransactionConfig>(1)?
            .unwrap_or_default();
        let encoding = config.encoding.unwrap_or(UiTransactionEncoding::Base58);

        let (transaction, remote_account_claims) = self
            .prepare_transaction(
                &transaction_str,
                encoding,
                TransactionKind::Simulate,
            )
            .await;
        *claims += remote_account_claims;
        let transaction = transaction?;
        let number_of_accounts = transaction.static_account_keys().len();

        let replacement_blockhash = config
            .replace_recent_blockhash
            .then(|| self.latest_blockhash().0);
        let inner_instructions_enabled = config.inner_instructions;
        let accounts_config = config.accounts;

        let record = self
            .engine
            .transaction(transaction)?
            .simulate()
            .await?
            .map_err(RpcError::transaction_simulation_from_scheduler)?;

        let result = record.result.flattened_result();
        let (logs, units_consumed, return_data, recorded_inner, post_accounts) =
            match record.result {
                Ok(executed) => {
                    let executed = *executed;
                    let details = executed.execution_details;
                    (
                        details.log_messages.map(|l| l.as_ref().clone()),
                        details.executed_units,
                        details.return_data,
                        details.inner_instructions,
                        executed.loaded_transaction.accounts,
                    )
                }
                Err(_) => (None, 0, None, None, Vec::new()),
            };

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
                let reader = |pubkey: &Pubkey, account: &AccountSharedData| {
                    encode_ui_account(
                        pubkey,
                        account,
                        accounts_encoding,
                        None,
                        None,
                    )
                };
                let (current_accounts, remote_account_claims) = self
                    .read_accounts_with_ensure(
                        &pubkeys,
                        AccountFetchContext::rpc_get_multiple_accounts(),
                        reader,
                    )
                    .await;
                *claims += remote_account_claims;
                let post_simulation_accounts =
                    post_accounts.into_iter().collect::<HashMap<_, _>>();

                Some(
                    pubkeys
                        .into_iter()
                        .zip(current_accounts)
                        .map(|(pubkey, account)| {
                            post_simulation_accounts
                                .get(&pubkey)
                                .cloned()
                                .map(|account| {
                                    encode_ui_account(
                                        &pubkey,
                                        &account,
                                        accounts_encoding,
                                        None,
                                        None,
                                    )
                                })
                                .or(account)
                        })
                        .collect(),
                )
            }
        } else {
            None
        };

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
            recorded_inner
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

        let slot = record.slot;
        Ok(ResponsePayload::encode(&request.id, result, slot))
    }
}
