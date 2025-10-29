use std::{
    future::Future, marker::PhantomData, ops::ControlFlow, time::Duration,
};

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

pub trait TransactionErrorMapper {
    type ExecutionError;
    fn try_map(
        &self,
        error: TransactionError,
        signature: Option<Signature>,
    ) -> Result<Self::ExecutionError, TransactionError>;
}

pub async fn send_transaction_with_retries<F, Fut, M, E1, E2>(
    make_send_fut: F,
    send_result_mapper: M,
    stop_predicate: impl Fn(usize, Duration) -> bool,
) -> Result<Signature, E2>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<MagicBlockSendTransactionOutcome, E1>>,
    M: SendErrorMapper<E1, ExecutionError = E2>,
    E1: From<MagicBlockRpcClientError>,
{
    let start = Instant::now();
    let mut i = 0;

    loop {
        i += 1;

        let result = make_send_fut().await;
        let err = match result {
            Ok(outcome) => match outcome.into_result_2() {
                Ok(signature) => return Ok(signature),
                Err(rpc_err) => E1::from(rpc_err),
            },
            Err(err) => err,
        };
        let mapped_error = send_result_mapper.map(err);
        let sleep_duration = match M::decide_flow(&mapped_error) {
            ControlFlow::Continue(value) => value,
            ControlFlow::Break(()) => return Err(mapped_error),
        };

        if stop_predicate(i, start.elapsed()) {
            return Err(mapped_error);
        }

        sleep(sleep_duration).await
    }
}

// E - Error after sending
// EM - Error mapped
pub struct DefaultErrorMapper<M, TM, E, EM>
where
    M: SendErrorMapper<E, ExecutionError = EM>,
    TM: TransactionErrorMapper<ExecutionError = EM>,
{
    pub inner_map: M,
    pub transaction_error_mapper: TM,
    _phantom_data: PhantomData<E>,
}

impl<M, TM, E, EM> SendErrorMapper<E> for DefaultErrorMapper<M, TM, E, EM>
where
    E: TryInto<MagicBlockRpcClientError, Error = E>,
    EM: From<MagicBlockRpcClientError>,
    M: SendErrorMapper<E, ExecutionError = EM>,
    TM: TransactionErrorMapper<ExecutionError = EM>,
    for<'a> &'a EM: TryInto<&'a MagicBlockRpcClientError, Error = &'a EM>,
{
    type ExecutionError = EM;

    fn map(&self, error: E) -> Self::ExecutionError {
        match error.try_into() {
            Ok(err) => self.map_magicblock_client_error(err),
            Err(err) => self.inner_map.map(err),
        }
    }

    fn decide_flow(
        mapped_error: &Self::ExecutionError,
    ) -> ControlFlow<(), Duration> {
        match mapped_error.try_into() {
            Ok(err) => Self::decide_rpc_error_flow(err),
            Err(mapped_error) => M::decide_flow(mapped_error),
        }
    }
}

impl<M, TM, E, EM> DefaultErrorMapper<M, TM, E, EM>
where
    M: SendErrorMapper<E, ExecutionError = EM>,
    TM: TransactionErrorMapper<ExecutionError = EM>,
    EM: From<MagicBlockRpcClientError>,
{
    pub fn new(inner_map: M, transaction_error_mapper: TM) -> Self {
        Self {
            inner_map,
            transaction_error_mapper,
            _phantom_data: Default::default(),
        }
    }

    fn map_magicblock_client_error(
        &self,
        error: MagicBlockRpcClientError,
    ) -> EM {
        match error {
            MagicBlockRpcClientError::SentTransactionError(
                transaction_err,
                signature,
            ) => {
                match self.transaction_error_mapper.try_map(transaction_err, Some(signature)) {
                    Ok(mapped_err) => mapped_err,
                    Err(original) => MagicBlockRpcClientError::SentTransactionError(
                        original,
                        signature,
                    ).into()
                }
            }
            MagicBlockRpcClientError::RpcClientError(err) => {
                match self.try_map_client_error(err) {
                    Ok(mapped_err) => mapped_err,
                    Err(original) => MagicBlockRpcClientError::RpcClientError(original).into()
                }
            }
            MagicBlockRpcClientError::SendTransaction(err) => {
                match self.try_map_client_error(err) {
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

    fn try_map_client_error(
        &self,
        err: solana_rpc_client_api::client_error::Error,
    ) -> Result<EM, solana_rpc_client_api::client_error::Error> {
        match err.kind {
            ErrorKind::TransactionError(transaction_err) => self
                .transaction_error_mapper
                .try_map(transaction_err, None)
                .map_err(|transaction_err| {
                    solana_rpc_client_api::client_error::Error {
                        request: err.request,
                        kind: ErrorKind::TransactionError(transaction_err),
                    }
                }),
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

    fn decide_rpc_error_flow(
        error: &MagicBlockRpcClientError,
    ) -> ControlFlow<(), Duration> {
        match error {
            MagicBlockRpcClientError::RpcClientError(err)
            | MagicBlockRpcClientError::SendTransaction(err)
            | MagicBlockRpcClientError::GetLatestBlockhash(err)=> {
                Self::decide_rpc_native_flow(err)
            }
            MagicBlockRpcClientError::GetSlot(_)
            | MagicBlockRpcClientError::LookupTableDeserialize(_)
            | MagicBlockRpcClientError::SentTransactionError(_, _)=> {
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
            },
        }
    }

    fn decide_rpc_native_flow(
        err: &solana_rpc_client_api::client_error::Error,
    ) -> ControlFlow<(), Duration> {
        match err.kind {
            // Retry IO errors
            ErrorKind::Io(_) => {
                ControlFlow::Continue(Duration::from_millis(500))
            }
            _ => {
                // Can't handle - propagate
                ControlFlow::Break(())
            }
        }
    }
}
