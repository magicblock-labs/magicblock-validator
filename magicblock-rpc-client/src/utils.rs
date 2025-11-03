use std::{future::Future, ops::ControlFlow, time::Duration};

use log::error;
use solana_rpc_client_api::client_error::ErrorKind;
use solana_sdk::signature::Signature;
use solana_transaction_error::TransactionError;
use tokio::time::{sleep, Instant};

use crate::{MagicBlockRpcClientError, MagicBlockSendTransactionOutcome};

pub trait SendErrorMapper<E> {
    type ExecutionError;
    fn map(&self, error: E) -> Self::ExecutionError;
    fn decide_flow(
        mapped_error: &Self::ExecutionError,
    ) -> ControlFlow<(), Duration>;
}

/// Sends a Solana transaction repeatedly until it succeeds, a stop condition is met,
/// or an unrecoverable error occurs.
///
/// This function encapsulates retry logic for sending a transaction asynchronously.
/// It retries according to the retry strategy defined by the [`SendErrorMapper`] and
/// the user-provided `stop_predicate` predicate.
///
/// # Type Parameters
///
/// - `Map`: A type implementing [`SendErrorMapper`] that maps lower-level send errors
///   (`SendErr`) to higher-level execution errors (`ExecErr`), and determines whether
///   to retry or stop based on the mapped error.
/// - `Stop`: A predicate function used to determine when to give up retrying.
/// - `SendError`: The error type returned by the Solana RPC client or a similar transport layer.
/// - `ExecErr`: The unified execution error type returned to the caller with mapped errors
pub async fn send_transaction_with_retries<F, Fut, Map, SendErr, ExecErr>(
    make_send_fut: F,
    send_result_mapper: Map,
    stop_predicate: impl Fn(usize, Duration) -> bool,
) -> Result<Signature, ExecErr>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<MagicBlockSendTransactionOutcome, SendErr>>,
    Map: SendErrorMapper<SendErr, ExecutionError = ExecErr>,
    SendErr: From<MagicBlockRpcClientError>,
{
    let start = Instant::now();
    let mut i = 0;

    loop {
        i += 1;

        let result = make_send_fut().await;
        let err = match result {
            Ok(outcome) => match outcome.into_result() {
                Ok(signature) => return Ok(signature),
                Err(rpc_err) => SendErr::from(rpc_err),
            },
            Err(err) => err,
        };
        let mapped_error = send_result_mapper.map(err);
        let sleep_duration = match Map::decide_flow(&mapped_error) {
            ControlFlow::Continue(value) => value,
            ControlFlow::Break(()) => return Err(mapped_error),
        };

        if stop_predicate(i, start.elapsed()) {
            return Err(mapped_error);
        }

        sleep(sleep_duration).await
    }
}

/// Maps Solana `TransactionError`s into a domain-specific execution error.
pub trait TransactionErrorMapper {
    type ExecutionError;

    /// Attempt to map a `TransactionError` into `Self::ExecutionError`.
    ///
    /// * `error` — The raw `TransactionError` produced by Solana.
    /// * `signature` — tx signature
    ///
    /// Return `Ok(mapped)` if you recognize and handle the error.
    /// Return `Err(original_error)` to indicate "not handled" so the caller can fall back.
    fn try_map(
        &self,
        error: TransactionError,
        signature: Option<Signature>,
    ) -> Result<Self::ExecutionError, TransactionError>;
}

/// Convert a `MagicBlockRpcClientError` into the caller’s execution error type using
/// a user-provided [`TransactionErrorMapper`], with safe fallbacks.
pub fn map_magicblock_client_error<TxMap, ExecErr>(
    transaction_error_mapper: &TxMap,
    error: MagicBlockRpcClientError,
) -> ExecErr
where
    TxMap: TransactionErrorMapper<ExecutionError = ExecErr>,
    ExecErr: From<MagicBlockRpcClientError>,
{
    match error {
        MagicBlockRpcClientError::SentTransactionError(
            transaction_err,
            signature,
        ) => {
            match transaction_error_mapper.try_map(transaction_err, Some(signature)) {
                Ok(mapped_err) => mapped_err,
                Err(original) => MagicBlockRpcClientError::SentTransactionError(
                    original,
                    signature,
                ).into()
            }
        }
        MagicBlockRpcClientError::RpcClientError(err) => {
            match try_map_client_error(transaction_error_mapper, err) {
                Ok(mapped_err) => mapped_err,
                Err(original) => MagicBlockRpcClientError::RpcClientError(original).into()
            }
        }
        MagicBlockRpcClientError::SendTransaction(err) => {
            match try_map_client_error(transaction_error_mapper, err) {
                Ok(mapped_err) => mapped_err,
                Err(original) => MagicBlockRpcClientError::SendTransaction(original).into()
            }
        }
        err @
        (MagicBlockRpcClientError::GetSlot(_)
        | MagicBlockRpcClientError::LookupTableDeserialize(_)) => {
            error!("Unexpected error during send transaction: {:?}", err);
            err.into()
        }
        err
        @ (MagicBlockRpcClientError::GetLatestBlockhash(_)
        | MagicBlockRpcClientError::CannotGetTransactionSignatureStatus(
            ..,
        )
        | MagicBlockRpcClientError::CannotConfirmTransactionSignatureStatus(
            ..,
        )) => err.into(),
    }
}

pub fn try_map_client_error<TxMap, ExecErr>(
    transaction_error_mapper: &TxMap,
    err: solana_rpc_client_api::client_error::Error,
) -> Result<ExecErr, solana_rpc_client_api::client_error::Error>
where
    TxMap: TransactionErrorMapper<ExecutionError = ExecErr>,
{
    match err.kind {
        ErrorKind::TransactionError(transaction_err) => {
            transaction_error_mapper
                .try_map(transaction_err, None)
                .map_err(|transaction_err| {
                    solana_rpc_client_api::client_error::Error {
                        request: err.request,
                        kind: ErrorKind::TransactionError(transaction_err),
                    }
                })
        }
        err_kind @ (ErrorKind::Reqwest(_)
        | ErrorKind::Middleware(_)
        | ErrorKind::RpcError(_)
        | ErrorKind::SerdeJson(_)
        | ErrorKind::SigningError(_)
        | ErrorKind::Custom(_)
        | ErrorKind::Io(_)) => {
            Err(solana_rpc_client_api::client_error::Error {
                request: err.request,
                kind: err_kind,
            })
        }
    }
}

pub fn decide_rpc_error_flow(
    error: &MagicBlockRpcClientError,
) -> ControlFlow<(), Duration> {
    match error {
        MagicBlockRpcClientError::RpcClientError(err)
        | MagicBlockRpcClientError::SendTransaction(err)
        | MagicBlockRpcClientError::GetLatestBlockhash(err) => {
            decide_rpc_native_flow(err)
        }
        MagicBlockRpcClientError::GetSlot(_)
        | MagicBlockRpcClientError::LookupTableDeserialize(_)
        | MagicBlockRpcClientError::SentTransactionError(_, _) => {
            // This wasn't mapped to any user defined error - break
            // Unexpected error - break
            ControlFlow::Break(())
        }
        MagicBlockRpcClientError::CannotGetTransactionSignatureStatus(..)
        | MagicBlockRpcClientError::CannotConfirmTransactionSignatureStatus(
            ..,
        ) => {
            // if there's still time left we can retry sending tx
            // Since [`DEFAULT_MAX_TIME_TO_PROCESSED`] is large we skip sleep as well
            ControlFlow::Continue(Duration::ZERO)
        }
    }
}

pub fn decide_rpc_native_flow(
    err: &solana_rpc_client_api::client_error::Error,
) -> ControlFlow<(), Duration> {
    match err.kind {
        // Retry IO errors
        ErrorKind::Io(_) => ControlFlow::Continue(Duration::from_millis(500)),
        _ => {
            // Can't handle - propagate
            ControlFlow::Break(())
        }
    }
}
