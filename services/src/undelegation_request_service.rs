use std::{fmt, sync::Arc, time::Duration};

use dlp_api::pda::undelegation_request_pda_from_delegated_account;
use engine::Engine;
use magicblock_chainlink::{
    AccountStatusOnEr, ObservedUndelegationRequest, ProdChainlink,
};
use magicblock_metrics::metrics::{
    AccountFetchContext, AccountFetchEntrypoint,
};
use magicblock_program::instruction_utils::InstructionUtils;
use nucleus::shutdown::{ShutdownHandle, ShutdownReason};
use solana_transaction_error::TransactionError;
use tokio::{
    sync::broadcast,
    task::{JoinError, JoinSet},
    time::MissedTickBehavior,
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

const UNDELEGATION_REQUEST_MAX_ATTEMPTS: usize = 3;
const UNDELEGATION_REQUEST_RETRY_BASE_DELAY: Duration =
    Duration::from_millis(100);

pub type ChainlinkImpl = ProdChainlink;

#[derive(Debug)]
enum ObservedUndelegationRequestError {
    Transient(&'static str),
    Schedule(TransactionError),
}

impl ObservedUndelegationRequestError {
    fn retryable(&self) -> bool {
        matches!(self, Self::Transient(_) | Self::Schedule(_))
    }
}

impl fmt::Display for ObservedUndelegationRequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transient(reason) => write!(f, "{reason}"),
            Self::Schedule(err) => {
                write!(f, "failed to schedule request: {err}")
            }
        }
    }
}

pub struct UndelegationRequestService {
    chainlink: Arc<ChainlinkImpl>,
    engine: Engine,
    undelegation_request_poll_interval: Duration,
}

impl UndelegationRequestService {
    pub fn new(
        chainlink: Arc<ChainlinkImpl>,
        engine: Engine,
        undelegation_request_poll_interval: Duration,
    ) -> Self {
        Self {
            chainlink,
            engine,
            undelegation_request_poll_interval,
        }
    }

    async fn undelegation_request_processor(
        mut requests: broadcast::Receiver<ObservedUndelegationRequest>,
        cancellation_token: CancellationToken,
        chainlink: Arc<ChainlinkImpl>,
        engine: Engine,
    ) {
        loop {
            let request = tokio::select! {
                biased;
                _ = cancellation_token.cancelled() => {
                    info!("Shutting down undelegation request processor");
                    return;
                }
                request = requests.recv() => {
                    match request {
                        Ok(request) => request,
                        Err(broadcast::error::RecvError::Closed) => {
                            info!("Undelegation request subscription closed");
                            return;
                        }
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            error!(
                                skipped_count = skipped,
                                skip_reason = "broadcast_receiver_lagged",
                                "Lagged behind undelegation request updates"
                            );
                            continue;
                        }
                    }
                }
            };

            Self::process_observed_undelegation_request_with_retries(
                request,
                &chainlink,
                &engine,
                &cancellation_token,
            )
            .await;
        }
    }

    async fn undelegation_request_poll_processor(
        poll_interval: Duration,
        cancellation_token: CancellationToken,
        chainlink: Arc<ChainlinkImpl>,
        engine: Engine,
    ) {
        if poll_interval.is_zero() {
            debug!(
                "DLP undelegation request polling is disabled by configuration"
            );
            return;
        }

        let mut interval = tokio::time::interval(poll_interval);
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                biased;
                _ = cancellation_token.cancelled() => {
                    info!("Shutting down undelegation request poll processor");
                    return;
                }
                _ = interval.tick() => {}
            }

            let requests = match chainlink.fetch_undelegation_requests().await {
                Ok(requests) => requests,
                Err(err) => {
                    error!(
                        error = ?err,
                        "Failed to scan DLP undelegation requests"
                    );
                    continue;
                }
            };
            for request in requests {
                Self::process_observed_undelegation_request_with_retries(
                    request,
                    &chainlink,
                    &engine,
                    &cancellation_token,
                )
                .await;
            }
        }
    }

    async fn process_observed_undelegation_request_with_retries(
        request: ObservedUndelegationRequest,
        chainlink: &ChainlinkImpl,
        engine: &Engine,
        cancellation_token: &CancellationToken,
    ) {
        let mut attempt = 1;
        loop {
            let result = Self::process_observed_undelegation_request(
                request.clone(),
                chainlink,
                engine,
            )
            .await;
            match result {
                Ok(()) => return,
                Err(err)
                    if err.retryable()
                        && attempt < UNDELEGATION_REQUEST_MAX_ATTEMPTS =>
                {
                    let delay = UNDELEGATION_REQUEST_RETRY_BASE_DELAY
                        * 2_u32.pow((attempt - 1) as u32);
                    warn!(
                        request_pda = %request.request_pda,
                        delegated_account = %request.delegated_account,
                        attempt,
                        max_attempts = UNDELEGATION_REQUEST_MAX_ATTEMPTS,
                        retry_delay_ms = delay.as_millis(),
                        error = %err,
                        "Retrying observed undelegation request after transient failure"
                    );
                    tokio::select! {
                        biased;
                        _ = cancellation_token.cancelled() => {
                            info!(
                                request_pda = %request.request_pda,
                                delegated_account = %request.delegated_account,
                                "Stopping undelegation request retry because processor is shutting down"
                            );
                            return;
                        }
                        _ = tokio::time::sleep(delay) => {}
                    }
                    attempt += 1;
                }
                Err(err) => {
                    error!(
                        request_pda = %request.request_pda,
                        delegated_account = %request.delegated_account,
                        attempt,
                        max_attempts = UNDELEGATION_REQUEST_MAX_ATTEMPTS,
                        error = %err,
                        "Failed to process observed undelegation request"
                    );
                    return;
                }
            }
        }
    }

    async fn process_observed_undelegation_request(
        request: ObservedUndelegationRequest,
        chainlink: &ChainlinkImpl,
        engine: &Engine,
    ) -> Result<(), ObservedUndelegationRequestError> {
        if request.observed_slot >= request.expires_at_slot {
            warn!(
                request_pda = %request.request_pda,
                delegated_account = %request.delegated_account,
                observed_slot = request.observed_slot,
                expires_at_slot = request.expires_at_slot,
                "Observed expired undelegation request; scheduling normal undelegation anyway to avoid timeout carry-over rollback when possible"
            );
        }

        let expected_request_pda =
            undelegation_request_pda_from_delegated_account(
                &request.delegated_account,
            );
        if expected_request_pda != request.request_pda {
            error!(
                request_pda = %request.request_pda,
                expected_request_pda = %expected_request_pda,
                delegated_account = %request.delegated_account,
                "Skipping undelegation request with invalid PDA"
            );
            return Ok(());
        }

        let mut delegation_status = match chainlink
            .account_delegation_statuses(
                &[request.delegated_account],
                AccountFetchContext::rpc_get_account(),
            )
            .await
        {
            Ok(value) => value.into_iter().next().unwrap_or_default(),
            Err(err) => {
                error!(
                    request_pda = %request.request_pda,
                    delegated_account = %request.delegated_account,
                    error = ?err,
                    "Failed to verify requested undelegation account"
                );
                return Err(ObservedUndelegationRequestError::Transient(
                    "failed to verify requested undelegation account",
                ));
            }
        };

        if delegation_status.delegated_on_base
            && delegation_status.account_on_er == AccountStatusOnEr::Missing
        {
            if let Err(err) = chainlink
                .ensure_accounts(
                    &[request.delegated_account],
                    AccountFetchEntrypoint::RpcGetAccount,
                )
                .await
            {
                error!(
                    request_pda = %request.request_pda,
                    delegated_account = %request.delegated_account,
                    error = ?err,
                    "Failed to materialize requested undelegation account"
                );
                return Err(ObservedUndelegationRequestError::Transient(
                    "failed to materialize requested undelegation account",
                ));
            }

            error!(
                request_pda = %request.request_pda,
                delegated_account = %request.delegated_account,
                delegated_on_base = delegation_status.delegated_on_base,
                account_on_er = delegation_status.account_on_er.as_str(),
                alert = "materialized_missing_er_account_for_undelegation_request",
                "Materialized delegated account for undelegation request because it was delegated on base but missing on ER"
            );

            delegation_status = match chainlink
                .account_delegation_statuses(
                    &[request.delegated_account],
                    AccountFetchContext::rpc_get_account(),
                )
                .await
            {
                Ok(value) => value.into_iter().next().unwrap_or_default(),
                Err(err) => {
                    error!(
                        request_pda = %request.request_pda,
                        delegated_account = %request.delegated_account,
                        error = ?err,
                        "Failed to verify materialized undelegation account"
                    );
                    return Err(ObservedUndelegationRequestError::Transient(
                        "failed to verify materialized undelegation account",
                    ));
                }
            };
        }

        let delegated_on_base_and_er = delegation_status.delegated_on_base
            && delegation_status.account_on_er.is_delegated();
        if !delegated_on_base_and_er {
            warn!(
                request_pda = %request.request_pda,
                delegated_account = %request.delegated_account,
                delegated_on_base = delegation_status.delegated_on_base,
                account_on_er = delegation_status.account_on_er.as_str(),
                skip_reason = delegation_status
                    .not_ready_reason()
                    .unwrap_or("delegation_status_ready"),
                "Skipping observed undelegation request because delegated account is not ready"
            );
            return Ok(());
        }

        if let Err(err) = chainlink
            .undelegation_requested(request.delegated_account)
            .await
        {
            error!(
                request_pda = %request.request_pda,
                delegated_account = %request.delegated_account,
                error = ?err,
                "Failed to start undelegation tracking"
            );
        }

        let ix = InstructionUtils::validator_schedule_commit_and_undelegate_instruction(
            &engine.authority(),
            vec![request.delegated_account],
        );
        let submitted = engine
            .transaction(&[ix])
            .map_err(|_| {
                ObservedUndelegationRequestError::Transient(
                    "failed to compose undelegation transaction",
                )
            })?
            .execute()
            .await
            .map_err(|_| {
                ObservedUndelegationRequestError::Transient(
                    "engine unavailable while scheduling undelegation",
                )
            })?;
        if let Err(err) = submitted {
            return Err(ObservedUndelegationRequestError::Schedule(err));
        }

        info!(
            request_pda = %request.request_pda,
            delegated_account = %request.delegated_account,
            delegated_on_base = delegation_status.delegated_on_base,
            account_on_er = delegation_status.account_on_er.as_str(),
            "Processed observed undelegation request and scheduled undelegation"
        );
        Ok(())
    }

    /// Runs and supervises the subscription and optional polling workers.
    pub async fn run(self, mut shutdown: ShutdownHandle) {
        let result = self.run_workers(shutdown.child()).await;
        let reason = match result {
            Err(error) => ShutdownReason::Error(Box::new(error)),
            Ok(()) if shutdown.requested() => ShutdownReason::Signalled,
            Ok(()) => ShutdownReason::Unexpected,
        };
        shutdown.terminate(reason);
    }

    async fn run_workers(
        self,
        cancellation_token: CancellationToken,
    ) -> Result<(), UndelegationRequestServiceError> {
        let mut workers = JoinSet::new();
        let requests = self.chainlink.subscribe_undelegation_requests();
        let token = cancellation_token.clone();
        let chainlink = self.chainlink.clone();
        let engine = self.engine.clone();
        workers.spawn(async move {
            Self::undelegation_request_processor(
                requests, token, chainlink, engine,
            )
            .await;
            "subscription processor"
        });

        if self.undelegation_request_poll_interval.is_zero() {
            debug!("Skipping DLP undelegation request poll processor");
        } else {
            let token = cancellation_token.clone();
            let chainlink = self.chainlink;
            let engine = self.engine;
            let poll_interval = self.undelegation_request_poll_interval;
            workers.spawn(async move {
                Self::undelegation_request_poll_processor(
                    poll_interval,
                    token,
                    chainlink,
                    engine,
                )
                .await;
                "poll processor"
            });
        }

        tokio::select! {
            biased;
            _ = cancellation_token.cancelled() => {}
            result = workers.join_next() => {
                let failure = match result.expect("undelegation workers are registered") {
                    Ok(worker) => UndelegationRequestServiceError::WorkerStopped(worker),
                    Err(error) => UndelegationRequestServiceError::WorkerJoin(error),
                };
                cancellation_token.cancel();
                workers.shutdown().await;
                return Err(failure);
            }
        }

        while let Some(result) = workers.join_next().await {
            if let Err(error) = result {
                workers.shutdown().await;
                return Err(error.into());
            }
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum UndelegationRequestServiceError {
    #[error("Undelegation request worker '{0}' stopped unexpectedly")]
    WorkerStopped(&'static str),
    #[error("Undelegation request worker failed: {0}")]
    WorkerJoin(#[from] JoinError),
}
