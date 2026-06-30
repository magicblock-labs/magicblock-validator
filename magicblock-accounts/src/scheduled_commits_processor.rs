use std::{
    collections::{HashMap, HashSet},
    fmt,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use dlp_api::pda::undelegation_request_pda_from_delegated_account;
use magicblock_account_cloner::ChainlinkCloner;
use magicblock_chainlink::{
    ObservedUndelegationRequest, ProdChainlink, ProdInnerChainlink,
};
use magicblock_core::{
    link::transactions::TransactionSchedulerHandle, traits::LatestBlockProvider,
};
use magicblock_metrics::metrics::AccountFetchOrigin;
use magicblock_program::{
    instruction::MagicBlockInstruction, MAGIC_CONTEXT_PUBKEY,
};
use solana_hash::Hash;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::Transaction;
use solana_transaction_error::TransactionError;
use tokio::{sync::broadcast, time::MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

const POISONED_MUTEX_MSG: &str =
    "Mutex of ScheduledCommitsProcessorImpl internal state is poisoned";
const UNDELEGATION_REQUEST_MAX_ATTEMPTS: usize = 3;
const UNDELEGATION_REQUEST_RETRY_BASE_DELAY: Duration =
    Duration::from_millis(100);

pub type InnerChainlinkImpl = ProdInnerChainlink<ChainlinkCloner>;
pub type ChainlinkImpl = ProdChainlink<ChainlinkCloner>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ObservedUndelegationRequestIdentity {
    created_slot: u64,
    expires_at_slot: u64,
    last_commit_id_at_request: u64,
    rent_payer: Pubkey,
}

impl From<&ObservedUndelegationRequest>
    for ObservedUndelegationRequestIdentity
{
    fn from(request: &ObservedUndelegationRequest) -> Self {
        Self {
            created_slot: request.created_slot,
            expires_at_slot: request.expires_at_slot,
            last_commit_id_at_request: request.last_commit_id_at_request,
            rent_payer: request.rent_payer,
        }
    }
}

type ObservedUndelegationRequests =
    HashMap<Pubkey, ObservedUndelegationRequestIdentity>;

#[derive(Debug)]
enum ObservedUndelegationRequestError {
    Transient(&'static str),
    Schedule(TransactionError),
    PoisonedMutex(&'static str),
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
            Self::PoisonedMutex(msg) => write!(f, "{msg}"),
        }
    }
}

pub struct ScheduledCommitsProcessorImpl {
    chainlink: Arc<ChainlinkImpl>,
    cancellation_token: CancellationToken,
    internal_transaction_scheduler: TransactionSchedulerHandle,
    validator_authority: Arc<Keypair>,
    latest_block_reader: Arc<dyn LatestBlockReader>,
    observed_undelegation_requests: Arc<Mutex<ObservedUndelegationRequests>>,
    undelegation_request_poll_interval: Duration,
}

trait LatestBlockReader: Send + Sync {
    fn blockhash(&self) -> Hash;
}

impl<T> LatestBlockReader for T
where
    T: LatestBlockProvider,
{
    fn blockhash(&self) -> Hash {
        LatestBlockProvider::blockhash(self)
    }
}

impl ScheduledCommitsProcessorImpl {
    pub fn new(
        chainlink: Arc<ChainlinkImpl>,
        internal_transaction_scheduler: TransactionSchedulerHandle,
        validator_authority: Keypair,
        latest_block: impl LatestBlockProvider,
        undelegation_request_poll_interval: Duration,
    ) -> Self {
        Self {
            chainlink,
            cancellation_token: CancellationToken::new(),
            internal_transaction_scheduler,
            validator_authority: Arc::new(validator_authority),
            latest_block_reader: Arc::new(latest_block.clone()),
            observed_undelegation_requests: Arc::new(Mutex::default()),
            undelegation_request_poll_interval,
        }
    }

    async fn undelegation_request_processor(
        mut requests: broadcast::Receiver<ObservedUndelegationRequest>,
        cancellation_token: CancellationToken,
        chainlink: Arc<ChainlinkImpl>,
        internal_transaction_scheduler: TransactionSchedulerHandle,
        validator_authority: Arc<Keypair>,
        latest_block: Arc<dyn LatestBlockReader>,
        observed_requests: Arc<Mutex<ObservedUndelegationRequests>>,
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
                &internal_transaction_scheduler,
                validator_authority.as_ref(),
                latest_block.as_ref(),
                &observed_requests,
                &cancellation_token,
            )
            .await;
        }
    }

    async fn undelegation_request_poll_processor(
        poll_interval: Duration,
        cancellation_token: CancellationToken,
        chainlink: Arc<ChainlinkImpl>,
        internal_transaction_scheduler: TransactionSchedulerHandle,
        validator_authority: Arc<Keypair>,
        latest_block: Arc<dyn LatestBlockReader>,
        observed_requests: Arc<Mutex<ObservedUndelegationRequests>>,
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
            let live_request_pdas = requests
                .iter()
                .map(|request| request.request_pda)
                .collect::<HashSet<_>>();

            for request in requests {
                Self::process_observed_undelegation_request_with_retries(
                    request,
                    &chainlink,
                    &internal_transaction_scheduler,
                    validator_authority.as_ref(),
                    latest_block.as_ref(),
                    &observed_requests,
                    &cancellation_token,
                )
                .await;
            }

            if let Err(err) = Self::retain_observed_requests(
                &observed_requests,
                &live_request_pdas,
            ) {
                error!(
                    error = %err,
                    "Failed to prune stale observed undelegation requests"
                );
            }
        }
    }

    async fn process_observed_undelegation_request_with_retries(
        request: ObservedUndelegationRequest,
        chainlink: &ChainlinkImpl,
        internal_transaction_scheduler: &TransactionSchedulerHandle,
        validator_authority: &Keypair,
        latest_block: &dyn LatestBlockReader,
        observed_requests: &Arc<Mutex<ObservedUndelegationRequests>>,
        cancellation_token: &CancellationToken,
    ) {
        let mut attempt = 1;
        loop {
            let result = Self::process_observed_undelegation_request(
                request.clone(),
                chainlink,
                internal_transaction_scheduler,
                validator_authority,
                latest_block,
                observed_requests,
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
        internal_transaction_scheduler: &TransactionSchedulerHandle,
        validator_authority: &Keypair,
        latest_block: &dyn LatestBlockReader,
        observed_requests: &Arc<Mutex<ObservedUndelegationRequests>>,
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

        let request_identity =
            ObservedUndelegationRequestIdentity::from(&request);
        {
            let mut observed = Self::lock_observed_requests(observed_requests)?;
            if observed.get(&request.request_pda) == Some(&request_identity) {
                debug!(
                    request_pda = %request.request_pda,
                    delegated_account = %request.delegated_account,
                    "Skipping already observed undelegation request"
                );
                return Ok(());
            }
            observed.insert(request.request_pda, request_identity);
        }

        let delegated = match chainlink
            .accounts_delegated_on_base_and_er(
                &[request.delegated_account],
                AccountFetchOrigin::GetAccount,
            )
            .await
        {
            Ok(value) => value.into_iter().next().unwrap_or(false),
            Err(err) => {
                Self::remove_observed_request(
                    observed_requests,
                    &request.request_pda,
                )?;
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
        if !delegated {
            Self::remove_observed_request(
                observed_requests,
                &request.request_pda,
            )?;
            debug!(
                request_pda = %request.request_pda,
                delegated_account = %request.delegated_account,
                "Skipping request because account is not delegated on base and ER"
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

        let ix = Self::schedule_commit_and_undelegate_instruction(
            &validator_authority.pubkey(),
            request.delegated_account,
        );
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&validator_authority.pubkey()),
            &[validator_authority],
            latest_block.blockhash(),
        );

        if let Err(err) = internal_transaction_scheduler.execute(tx).await {
            Self::remove_observed_request(
                observed_requests,
                &request.request_pda,
            )?;
            return Err(ObservedUndelegationRequestError::Schedule(err));
        }

        info!(
            request_pda = %request.request_pda,
            delegated_account = %request.delegated_account,
            "Scheduled requested undelegation via ScheduleCommitAndUndelegate"
        );
        Ok(())
    }

    fn lock_observed_requests<'a>(
        observed_requests: &'a Arc<Mutex<ObservedUndelegationRequests>>,
    ) -> Result<
        MutexGuard<'a, ObservedUndelegationRequests>,
        ObservedUndelegationRequestError,
    > {
        observed_requests.lock().map_err(|_| {
            ObservedUndelegationRequestError::PoisonedMutex(POISONED_MUTEX_MSG)
        })
    }

    fn remove_observed_request(
        observed_requests: &Arc<Mutex<ObservedUndelegationRequests>>,
        request_pda: &Pubkey,
    ) -> Result<(), ObservedUndelegationRequestError> {
        Self::lock_observed_requests(observed_requests)?.remove(request_pda);
        Ok(())
    }

    fn retain_observed_requests(
        observed_requests: &Arc<Mutex<ObservedUndelegationRequests>>,
        live_request_pdas: &HashSet<Pubkey>,
    ) -> Result<(), ObservedUndelegationRequestError> {
        Self::lock_observed_requests(observed_requests)?
            .retain(|request_pda, _| live_request_pdas.contains(request_pda));
        Ok(())
    }

    fn schedule_commit_and_undelegate_instruction(
        payer: &Pubkey,
        delegated_account: Pubkey,
    ) -> Instruction {
        Instruction::new_with_bincode(
            magicblock_program::id(),
            &MagicBlockInstruction::ScheduleCommitAndUndelegate,
            vec![
                AccountMeta::new(*payer, true),
                AccountMeta::new(MAGIC_CONTEXT_PUBKEY, false),
                AccountMeta::new(delegated_account, false),
            ],
        )
    }

    pub fn spawn_undelegation_request_processor(self: &Arc<Self>) {
        let Some(requests) = self.chainlink.subscribe_undelegation_requests()
        else {
            if !self.undelegation_request_poll_interval.is_zero() {
                warn!(
                    "Cannot subscribe to DLP undelegation requests; falling back to polling only"
                );
            }
            self.spawn_undelegation_request_poll_processor();
            return;
        };
        tokio::spawn(Self::undelegation_request_processor(
            requests,
            self.cancellation_token.clone(),
            self.chainlink.clone(),
            self.internal_transaction_scheduler.clone(),
            self.validator_authority.clone(),
            self.latest_block_reader.clone(),
            self.observed_undelegation_requests.clone(),
        ));
        self.spawn_undelegation_request_poll_processor();
    }

    fn spawn_undelegation_request_poll_processor(self: &Arc<Self>) {
        if self.undelegation_request_poll_interval.is_zero() {
            debug!("Skipping DLP undelegation request poll processor");
            return;
        }
        tokio::spawn(Self::undelegation_request_poll_processor(
            self.undelegation_request_poll_interval,
            self.cancellation_token.clone(),
            self.chainlink.clone(),
            self.internal_transaction_scheduler.clone(),
            self.validator_authority.clone(),
            self.latest_block_reader.clone(),
            self.observed_undelegation_requests.clone(),
        ));
    }

    pub fn stop(&self) {
        self.cancellation_token.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retain_observed_requests_prunes_missing_scan_results() {
        let keep = Pubkey::new_unique();
        let prune = Pubkey::new_unique();
        let observed_requests: Arc<Mutex<ObservedUndelegationRequests>> =
            Arc::new(Mutex::new(HashMap::from([
                (
                    keep,
                    ObservedUndelegationRequestIdentity {
                        created_slot: 1,
                        expires_at_slot: 2,
                        last_commit_id_at_request: 3,
                        rent_payer: Pubkey::new_unique(),
                    },
                ),
                (
                    prune,
                    ObservedUndelegationRequestIdentity {
                        created_slot: 4,
                        expires_at_slot: 5,
                        last_commit_id_at_request: 6,
                        rent_payer: Pubkey::new_unique(),
                    },
                ),
            ])));

        ScheduledCommitsProcessorImpl::retain_observed_requests(
            &observed_requests,
            &HashSet::from([keep]),
        )
        .unwrap();

        let observed = observed_requests.lock().unwrap();
        assert!(observed.contains_key(&keep));
        assert!(!observed.contains_key(&prune));
    }
}
