use std::sync::Arc;

use base64::{Engine, prelude::BASE64_STANDARD};
use magicblock_chainlink::errors::ChainlinkResult;
use magicblock_metrics::metrics::{
    AccountFetchContext, ENSURE_ACCOUNTS_TIME, TRANSACTION_PROCESSING_TIME,
    TRANSACTION_SKIP_PREFLIGHT,
};
use nucleus::runtime::TransactionView;
use solana_rpc_client_api::config::RpcSendTransactionConfig;
use solana_transaction_error::TransactionError;
use solana_transaction_status::UiTransactionEncoding;
use tracing::warn;

use super::ClaimedHandlerResult;
use crate::{
    RpcResult,
    error::RpcError,
    requests::{
        JsonHttpRequest as JsonRequest, params::SerdeSignature,
        payload::ResponsePayload,
    },
    server::http::dispatch::HttpDispatcher,
};

#[derive(Clone, Copy)]
pub(super) enum TransactionKind {
    Send,
    Simulate,
}

impl HttpDispatcher {
    pub(super) async fn prepare_transaction(
        &self,
        transaction: &str,
        encoding: UiTransactionEncoding,
        kind: TransactionKind,
    ) -> (RpcResult<TransactionView>, u64) {
        let bytes = match encoding {
            UiTransactionEncoding::Base58 => bs58::decode(transaction)
                .into_vec()
                .map_err(RpcError::parse_error),
            UiTransactionEncoding::Base64 => BASE64_STANDARD
                .decode(transaction)
                .map_err(RpcError::parse_error),
            _ => {
                return (
                    Err(RpcError::invalid_params(
                        "unsupported transaction encoding",
                    )),
                    0,
                );
            }
        };
        let bytes = match bytes {
            Ok(bytes) => bytes,
            Err(error) => return (Err(error), 0),
        };
        let transaction =
            TransactionView::try_new_sanitized(Arc::new(bytes), true).map_err(
                |error| RpcError::invalid_params(format!("{error:?}")),
            );
        let transaction = match transaction {
            Ok(transaction) => transaction,
            Err(error) => return (Err(error), 0),
        };
        let signature = transaction.signatures()[0];
        let fetch_context = match kind {
            TransactionKind::Send => {
                AccountFetchContext::send_transaction(signature)
            }
            TransactionKind::Simulate => {
                AccountFetchContext::simulate_transaction(signature)
            }
        };
        let outcome = self
            .ensure_transaction_accounts(&transaction, fetch_context)
            .await;
        match outcome {
            Ok(claims) => (Ok(transaction), claims),
            Err(error) => (Err(RpcError::transaction_verification(error)), 0),
        }
    }

    async fn ensure_transaction_accounts(
        &self,
        transaction: &TransactionView,
        fetch_context: AccountFetchContext,
    ) -> ChainlinkResult<u64> {
        let _timer = ENSURE_ACCOUNTS_TIME
            .with_label_values(&["transaction"])
            .start_timer();
        let outcome = self
            .chainlink
            .ensure_transaction_accounts_with_context(
                transaction,
                fetch_context,
            )
            .await;
        if let Err(error) = &outcome {
            warn!(?error, "failed to ensure transaction accounts");
        }
        outcome
    }

    pub(crate) async fn send_transaction(
        &self,
        request: &JsonRequest,
    ) -> ClaimedHandlerResult {
        let mut claims = 0;
        let result = async {
            let _timer = TRANSACTION_PROCESSING_TIME.start_timer();
            let transaction_str = request.required::<String>(0)?;
            let config = request
                .optional::<RpcSendTransactionConfig>(1)?
                .unwrap_or_default();
            let encoding =
                config.encoding.unwrap_or(UiTransactionEncoding::Base58);

            let (transaction, remote_account_claims) = self
                .prepare_transaction(
                    &transaction_str,
                    encoding,
                    TransactionKind::Send,
                )
                .await;
            claims += remote_account_claims;
            let transaction = transaction?;
            let signature = transaction.signatures()[0];

            if config.skip_preflight {
                TRANSACTION_SKIP_PREFLIGHT.inc();
                self.engine.transaction(transaction)?.schedule().await?;
            } else {
                self.engine.transaction(transaction)?.execute().await??;
            }

            let signature = SerdeSignature(signature);
            Ok(ResponsePayload::encode_no_context(&request.id, signature))
        }
        .await;
        (result, claims)
    }
}
