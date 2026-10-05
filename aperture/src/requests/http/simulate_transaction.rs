use crate::account::TransactionKind;
use crate::transaction::live_inner_instructions;
use std::{collections::HashMap, sync::Arc};

use magicblock_metrics::metrics::AccountFetchEntrypoint;
use solana_account::AccountSharedData;
use solana_account_decoder::{UiAccountEncoding, encode_ui_account};
use solana_pubkey::Pubkey;
use solana_rpc_client_api::{
    config::RpcSimulateTransactionConfig, response::RpcSimulateTransactionResult,
};
use solana_svm::transaction_processing_result::TransactionProcessingResultExtensions;
use solana_transaction_status::UiTransactionEncoding;

use super::{HandlerResult, RpcHandlers};
use crate::{
    error::RpcError,
    requests::{JsonHttpRequest as JsonRequest, payload::ResponsePayload},
};

impl RpcHandlers {
    pub(crate) async fn simulate_transaction(
        &self,
        request: &JsonRequest,
        claims: &mut u64,
    ) -> HandlerResult {
        let transaction_str = request.required::<&str>(0)?;
        let config = request.optional::<RpcSimulateTransactionConfig>(1)?.unwrap_or_default();
        let encoding = config.encoding.unwrap_or(UiTransactionEncoding::Base58);

        let transaction = self
            .accounts
            .prepare_transaction(transaction_str, encoding, TransactionKind::Simulate, claims)
            .await?;
        let number_of_accounts = transaction.static_account_keys().len();

        let replacement_blockhash =
            config.replace_recent_blockhash.then(|| self.latest_blockhash().0);
        let inner_instructions_enabled = config.inner_instructions;
        let accounts_config = config.accounts;

        let record = self
            .engine
            .transaction(transaction)?
            .simulate()
            .await?
            .map_err(RpcError::transaction_simulation_from_scheduler)?;

        let result = record.result.flattened_result();
        let (logs, units_consumed, return_data, recorded_inner, post_accounts) = match record.result
        {
            Ok(executed) => {
                let executed = *executed;
                let details = executed.execution_details;
                (
                    details.log_messages.map(Arc::unwrap_or_clone),
                    details.executed_units,
                    details.return_data,
                    details.inner_instructions,
                    executed.loaded_transaction.accounts,
                )
            }
            Err(_) => (None, 0, None, None, Vec::new()),
        };

        let accounts = if let Some(config_accounts) = accounts_config {
            let accounts_encoding = config_accounts.encoding.unwrap_or(UiAccountEncoding::Base64);

            if accounts_encoding == UiAccountEncoding::Binary
                || accounts_encoding == UiAccountEncoding::Base58
            {
                return Err(RpcError::invalid_params("base58 encoding not supported"));
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
                    .map(|address| address.parse::<Pubkey>().map_err(RpcError::invalid_params))
                    .collect::<Result<Vec<_>, _>>()?;
                // Keep synchronization and claim accounting even for simulated images.
                self.accounts
                    .ensure(
                        &pubkeys,
                        AccountFetchEntrypoint::RpcGetMultipleAccounts,
                        claims,
                    )
                    .await;
                let post_accounts = post_accounts.into_iter().collect::<HashMap<_, _>>();
                let accessor = self.engine.accounts();
                let loader = accessor.loader();
                let mut accounts = Vec::with_capacity(pubkeys.len());
                for pubkey in pubkeys {
                    let encode = |account: &AccountSharedData| {
                        encode_ui_account(&pubkey, account, accounts_encoding, None, None)
                    };
                    // Read current state only when simulation did not return this address.
                    let account = match post_accounts.get(&pubkey) {
                        Some(account) => Some(encode(account)),
                        None => loader.read(&pubkey, encode).ok().flatten(),
                    };
                    accounts.push(account);
                }
                Some(accounts)
            }
        } else {
            None
        };

        let inner_instructions = inner_instructions_enabled.then(|| {
            live_inner_instructions(recorded_inner.unwrap_or_default())
                .into_iter()
                .map(Into::into)
                .collect()
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
        ResponsePayload::encode(&request.id, result, slot)
    }
}
