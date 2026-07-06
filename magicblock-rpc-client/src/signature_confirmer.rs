use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use futures_util::StreamExt;
use magicblock_metrics::metrics;
use solana_commitment_config::CommitmentConfig;
use solana_pubsub_client::nonblocking::pubsub_client::PubsubClient;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_rpc_client_api::{
    config::RpcSignatureSubscribeConfig, response::RpcSignatureResult,
};
use solana_signature::Signature;
use solana_transaction_error::{TransactionError, TransactionResult};
use solana_transaction_status_client_types::TransactionStatus;
use tokio::{
    sync::{Mutex, oneshot},
    time::{sleep, timeout},
};
use tracing::*;

const DEFAULT_SIGNATURE_STATUS_BATCH_SIZE: usize = 256;
const DEFAULT_STATUS_CACHE_TTL: Duration = Duration::from_secs(30);
const DEFAULT_WS_FALLBACK_DELAY: Duration = Duration::from_secs(2);
const DEFAULT_POLL_COALESCE_DELAY: Duration = Duration::from_millis(25);

#[derive(Clone, Debug)]
pub(crate) struct SignatureConfirmerConfig {
    pub(crate) websocket_url: Option<String>,
    pub(crate) batch_size: usize,
    pub(crate) status_cache_ttl: Duration,
    pub(crate) websocket_fallback_delay: Duration,
    pub(crate) poll_coalesce_delay: Duration,
}

impl Default for SignatureConfirmerConfig {
    fn default() -> Self {
        Self {
            websocket_url: None,
            batch_size: DEFAULT_SIGNATURE_STATUS_BATCH_SIZE,
            status_cache_ttl: DEFAULT_STATUS_CACHE_TTL,
            websocket_fallback_delay: DEFAULT_WS_FALLBACK_DELAY,
            poll_coalesce_delay: DEFAULT_POLL_COALESCE_DELAY,
        }
    }
}

impl SignatureConfirmerConfig {
    pub(crate) fn with_websocket_url(websocket_url: Option<String>) -> Self {
        Self {
            websocket_url,
            ..Self::default()
        }
    }
}

#[derive(Clone)]
pub(crate) struct SignatureConfirmer {
    rpc_client: Arc<RpcClient>,
    config: SignatureConfirmerConfig,
    poll_state: Arc<Mutex<PollState>>,
    pubsub_client: Arc<Mutex<Option<Arc<PubsubClient>>>>,
    next_waiter_id: Arc<AtomicU64>,
}

impl SignatureConfirmer {
    pub(crate) fn new(
        rpc_client: Arc<RpcClient>,
        config: SignatureConfirmerConfig,
    ) -> Self {
        let mut config = config;
        if config.batch_size == 0 {
            config.batch_size = DEFAULT_SIGNATURE_STATUS_BATCH_SIZE;
        }
        Self {
            rpc_client,
            config,
            poll_state: Arc::new(Mutex::new(PollState::default())),
            pubsub_client: Arc::new(Mutex::new(None)),
            next_waiter_id: Arc::new(AtomicU64::new(1)),
        }
    }

    pub(crate) async fn wait_for_status(
        &self,
        signature: &Signature,
        commitment: CommitmentConfig,
        timeout_duration: Duration,
        poll_interval: Duration,
    ) -> Option<TransactionResult<()>> {
        if timeout_duration.is_zero() {
            return self.cached_status(signature, commitment).await;
        }

        if self.config.websocket_url.is_some() {
            self.wait_with_websocket_then_poll(
                signature,
                commitment,
                timeout_duration,
                poll_interval,
            )
            .await
        } else {
            self.wait_with_batched_polling(
                signature,
                commitment,
                timeout_duration,
                poll_interval,
            )
            .await
        }
    }

    async fn wait_with_websocket_then_poll(
        &self,
        signature: &Signature,
        commitment: CommitmentConfig,
        timeout_duration: Duration,
        poll_interval: Duration,
    ) -> Option<TransactionResult<()>> {
        let start = Instant::now();
        let fallback_delay = std::cmp::min(
            timeout_duration,
            self.config.websocket_fallback_delay,
        );
        let pubsub_client = match timeout(fallback_delay, self.pubsub_client())
            .await
        {
            Ok(Some(pubsub_client)) => pubsub_client,
            Ok(None) => {
                metrics::inc_rpc_client_signature_ws_fallback_count();
                return self
                    .wait_with_batched_polling(
                        signature,
                        commitment,
                        timeout_duration.saturating_sub(start.elapsed()),
                        poll_interval,
                    )
                    .await;
            }
            Err(_) => {
                warn!(
                    signature = %signature,
                    "Signature confirmation websocket connection timed out; falling back to batched polling"
                );
                metrics::inc_rpc_client_signature_ws_fallback_count();
                return self
                    .wait_with_batched_polling(
                        signature,
                        commitment,
                        timeout_duration.saturating_sub(start.elapsed()),
                        poll_interval,
                    )
                    .await;
            }
        };

        let config = RpcSignatureSubscribeConfig {
            commitment: Some(commitment),
            enable_received_notification: Some(false),
        };
        let subscribe_timeout = fallback_delay.saturating_sub(start.elapsed());
        let (mut notifications, unsubscribe) = match timeout(
            subscribe_timeout,
            pubsub_client.signature_subscribe(signature, Some(config)),
        )
        .await
        {
            Ok(Ok(subscription)) => subscription,
            Ok(Err(err)) => {
                warn!(
                    signature = %signature,
                    error = ?err,
                    "Signature confirmation websocket subscription failed; falling back to batched polling"
                );
                metrics::inc_rpc_client_signature_ws_fallback_count();
                self.clear_pubsub_client().await;
                return self
                    .wait_with_batched_polling(
                        signature,
                        commitment,
                        timeout_duration,
                        poll_interval,
                    )
                    .await;
            }
            Err(_) => {
                warn!(
                    signature = %signature,
                    "Signature confirmation websocket subscription timed out; falling back to batched polling"
                );
                metrics::inc_rpc_client_signature_ws_fallback_count();
                return self
                    .wait_with_batched_polling(
                        signature,
                        commitment,
                        timeout_duration.saturating_sub(start.elapsed()),
                        poll_interval,
                    )
                    .await;
            }
        };
        let mut unsubscribe = Some(unsubscribe);
        metrics::inc_rpc_client_signature_ws_subscribe_count();

        let fallback_delay = fallback_delay.saturating_sub(start.elapsed());
        let fallback = async {
            sleep(fallback_delay).await;
            metrics::inc_rpc_client_signature_ws_fallback_count();
            let remaining = timeout_duration.saturating_sub(start.elapsed());
            self.wait_with_batched_polling(
                signature,
                commitment,
                remaining,
                poll_interval,
            )
            .await
        };
        tokio::pin!(fallback);

        loop {
            tokio::select! {
                notification = notifications.next() => {
                    match notification {
                        Some(notification) => {
                            if let Some(status) = Self::status_from_websocket_notification(notification.value) {
                                if let Some(unsubscribe) = unsubscribe.take() {
                                    unsubscribe().await;
                                }
                                metrics::inc_rpc_client_signature_ws_notification_count();
                                return Some(status);
                            }
                        }
                        None => {
                            if let Some(unsubscribe) = unsubscribe.take() {
                                unsubscribe().await;
                            }
                            let remaining = timeout_duration.saturating_sub(start.elapsed());
                            return self
                                .wait_with_batched_polling(
                                    signature,
                                    commitment,
                                    remaining,
                                    poll_interval,
                                )
                                .await;
                        }
                    }
                }
                status = &mut fallback => {
                    if let Some(unsubscribe) = unsubscribe.take() {
                        unsubscribe().await;
                    }
                    return status;
                }
            }
        }
    }

    fn status_from_websocket_notification(
        notification: RpcSignatureResult,
    ) -> Option<TransactionResult<()>> {
        match notification {
            RpcSignatureResult::ProcessedSignature(status) => {
                Some(match status.err {
                    Some(err) => Err(TransactionError::from(err)),
                    None => Ok(()),
                })
            }
            RpcSignatureResult::ReceivedSignature(_) => None,
        }
    }

    async fn pubsub_client(&self) -> Option<Arc<PubsubClient>> {
        let websocket_url = self.config.websocket_url.as_ref()?;
        let mut cached = self.pubsub_client.lock().await;
        if let Some(client) = cached.as_ref() {
            return Some(client.clone());
        }

        match PubsubClient::new(websocket_url).await {
            Ok(client) => {
                let client = Arc::new(client);
                *cached = Some(client.clone());
                Some(client)
            }
            Err(err) => {
                warn!(
                    error = ?err,
                    "Signature confirmation websocket connection failed; using batched polling"
                );
                None
            }
        }
    }

    async fn clear_pubsub_client(&self) {
        let mut cached = self.pubsub_client.lock().await;
        *cached = None;
    }

    async fn wait_with_batched_polling(
        &self,
        signature: &Signature,
        commitment: CommitmentConfig,
        timeout_duration: Duration,
        poll_interval: Duration,
    ) -> Option<TransactionResult<()>> {
        if timeout_duration.is_zero() {
            return self.cached_status(signature, commitment).await;
        }

        let waiter_id = self.next_waiter_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        let start_worker = {
            let mut state = self.poll_state.lock().await;
            state.prune_status_cache(self.config.status_cache_ttl);
            if let Some(status) = state.cached_status(signature, commitment) {
                return Some(status);
            }
            state
                .waiters
                .entry(*signature)
                .or_default()
                .push(StatusWaiter {
                    id: waiter_id,
                    commitment,
                    sender,
                });
            if state.worker_running {
                false
            } else {
                state.worker_running = true;
                true
            }
        };

        if start_worker {
            let confirmer = self.clone();
            tokio::spawn(async move {
                confirmer.poll_loop(poll_interval).await;
            });
        }

        match timeout(timeout_duration, receiver).await {
            Ok(Ok(status)) => Some(status),
            Ok(Err(_)) => None,
            Err(_) => {
                self.remove_waiter(signature, waiter_id).await;
                None
            }
        }
    }

    async fn cached_status(
        &self,
        signature: &Signature,
        commitment: CommitmentConfig,
    ) -> Option<TransactionResult<()>> {
        let mut state = self.poll_state.lock().await;
        state.prune_status_cache(self.config.status_cache_ttl);
        state.cached_status(signature, commitment)
    }

    async fn remove_waiter(&self, signature: &Signature, waiter_id: u64) {
        let mut state = self.poll_state.lock().await;
        if let Some(waiters) = state.waiters.get_mut(signature) {
            waiters.retain(|waiter| waiter.id != waiter_id);
            if waiters.is_empty() {
                state.waiters.remove(signature);
            }
        }
    }

    async fn poll_loop(self, poll_interval: Duration) {
        let mut delay = self.config.poll_coalesce_delay;
        loop {
            sleep(delay).await;
            let signatures = {
                let mut state = self.poll_state.lock().await;
                state.prune_status_cache(self.config.status_cache_ttl);
                if state.waiters.is_empty() {
                    state.worker_running = false;
                    return;
                }
                state.waiters.keys().copied().collect::<Vec<_>>()
            };

            let statuses = self.fetch_statuses(&signatures).await;
            if !statuses.is_empty() {
                self.apply_statuses(statuses).await;
            }
            delay = poll_interval;
        }
    }

    async fn fetch_statuses(
        &self,
        signatures: &[Signature],
    ) -> Vec<(Signature, TransactionStatus)> {
        let mut fetched = Vec::new();
        for chunk in signatures.chunks(self.config.batch_size) {
            metrics::inc_rpc_client_signature_status_batch_count();
            metrics::inc_rpc_client_signature_status_batch_signatures_count(
                chunk.len() as u64,
            );
            match self.rpc_client.get_signature_statuses(chunk).await {
                Ok(response) => {
                    fetched.extend(
                        chunk.iter().copied().zip(response.value).filter_map(
                            |(signature, status)| {
                                status.map(|status| (signature, status))
                            },
                        ),
                    );
                }
                Err(err) => {
                    trace!(
                        error = ?err,
                        signatures = chunk.len(),
                        "Failed to fetch batched signature statuses"
                    );
                }
            }
        }
        fetched
    }

    async fn apply_statuses(
        &self,
        statuses: Vec<(Signature, TransactionStatus)>,
    ) {
        let mut state = self.poll_state.lock().await;
        let now = Instant::now();
        for (signature, status) in statuses {
            state.cached_statuses.insert(
                signature,
                CachedSignatureStatus {
                    status: status.clone(),
                    fetched_at: now,
                },
            );

            let Some(waiters) = state.waiters.remove(&signature) else {
                continue;
            };
            let mut pending = Vec::new();
            for waiter in waiters {
                if let Some(result) =
                    status_result_for_commitment(&status, waiter.commitment)
                {
                    let _ = waiter.sender.send(result);
                } else {
                    pending.push(waiter);
                }
            }
            if !pending.is_empty() {
                state.waiters.insert(signature, pending);
            }
        }
    }
}

#[derive(Default)]
struct PollState {
    waiters: HashMap<Signature, Vec<StatusWaiter>>,
    cached_statuses: HashMap<Signature, CachedSignatureStatus>,
    worker_running: bool,
}

impl PollState {
    fn cached_status(
        &self,
        signature: &Signature,
        commitment: CommitmentConfig,
    ) -> Option<TransactionResult<()>> {
        self.cached_statuses.get(signature).and_then(|status| {
            status_result_for_commitment(&status.status, commitment)
        })
    }

    fn prune_status_cache(&mut self, ttl: Duration) {
        self.cached_statuses
            .retain(|_, status| status.fetched_at.elapsed() < ttl);
    }
}

struct StatusWaiter {
    id: u64,
    commitment: CommitmentConfig,
    sender: oneshot::Sender<TransactionResult<()>>,
}

struct CachedSignatureStatus {
    status: TransactionStatus,
    fetched_at: Instant,
}

fn status_result_for_commitment(
    status: &TransactionStatus,
    commitment: CommitmentConfig,
) -> Option<TransactionResult<()>> {
    if status.satisfies_commitment(commitment) {
        Some(status.status.clone())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Mutex as StdMutex,
        atomic::{AtomicUsize, Ordering},
    };

    use async_trait::async_trait;
    use serde_json::Value;
    use solana_rpc_client::{
        rpc_client::RpcClientConfig,
        rpc_sender::{RpcSender, RpcTransportStats},
    };
    use solana_rpc_client_api::{
        client_error::Result as RpcResult,
        request::RpcRequest,
        response::{Response, RpcResponseContext},
    };
    use solana_transaction_status_client_types::TransactionConfirmationStatus;

    use super::*;

    #[tokio::test]
    async fn batches_concurrent_waiters() {
        let sender = RecordingSender::ok();
        let calls = sender.calls.clone();
        let batch_sizes = sender.batch_sizes.clone();
        let confirmer = test_confirmer(sender);
        let signature_a = test_signature(1);
        let signature_b = test_signature(2);

        let (status_a, status_b) = tokio::join!(
            confirmer.wait_for_status(
                &signature_a,
                CommitmentConfig::processed(),
                Duration::from_secs(1),
                Duration::from_millis(20),
            ),
            confirmer.wait_for_status(
                &signature_b,
                CommitmentConfig::processed(),
                Duration::from_secs(1),
                Duration::from_millis(20),
            ),
        );

        assert_eq!(status_a, Some(Ok(())));
        assert_eq!(status_b, Some(Ok(())));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(*batch_sizes.lock().unwrap(), vec![2]);
    }

    #[tokio::test]
    async fn caches_completed_statuses() {
        let sender = RecordingSender::ok();
        let calls = sender.calls.clone();
        let confirmer = test_confirmer(sender);
        let signature = test_signature(1);

        let first = confirmer
            .wait_for_status(
                &signature,
                CommitmentConfig::processed(),
                Duration::from_secs(1),
                Duration::from_millis(20),
            )
            .await;
        let second = confirmer
            .wait_for_status(
                &signature,
                CommitmentConfig::confirmed(),
                Duration::from_secs(1),
                Duration::from_millis(20),
            )
            .await;

        assert_eq!(first, Some(Ok(())));
        assert_eq!(second, Some(Ok(())));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn transaction_errors_wait_for_requested_commitment() {
        let err = TransactionError::AccountNotFound;
        let status = TransactionStatus {
            status: Err(err.clone()),
            slot: 1,
            confirmations: Some(1),
            err: Some(err.clone()),
            confirmation_status: Some(TransactionConfirmationStatus::Processed),
        };

        assert_eq!(
            status_result_for_commitment(
                &status,
                CommitmentConfig::confirmed()
            ),
            None
        );
        assert_eq!(
            status_result_for_commitment(
                &status,
                CommitmentConfig::processed()
            ),
            Some(Err(err))
        );
    }

    fn test_confirmer(sender: RecordingSender) -> SignatureConfirmer {
        let rpc_client =
            RpcClient::new_sender(sender, RpcClientConfig::default());
        SignatureConfirmer::new(
            Arc::new(rpc_client),
            SignatureConfirmerConfig {
                poll_coalesce_delay: Duration::from_millis(5),
                ..SignatureConfirmerConfig::default()
            },
        )
    }

    fn test_signature(byte: u8) -> Signature {
        Signature::from([byte; 64])
    }

    #[derive(Clone)]
    struct RecordingSender {
        calls: Arc<AtomicUsize>,
        batch_sizes: Arc<StdMutex<Vec<usize>>>,
        status: TransactionStatus,
    }

    impl RecordingSender {
        fn ok() -> Self {
            let status = Ok(());
            Self {
                calls: Arc::new(AtomicUsize::new(0)),
                batch_sizes: Arc::new(StdMutex::new(Vec::new())),
                status: TransactionStatus {
                    status: status.clone(),
                    slot: 1,
                    confirmations: None,
                    err: status.err(),
                    confirmation_status: Some(
                        TransactionConfirmationStatus::Finalized,
                    ),
                },
            }
        }
    }

    #[async_trait]
    impl RpcSender for RecordingSender {
        async fn send(
            &self,
            request: RpcRequest,
            params: Value,
        ) -> RpcResult<Value> {
            assert_eq!(request, RpcRequest::GetSignatureStatuses);
            self.calls.fetch_add(1, Ordering::Relaxed);
            let signature_count =
                params.as_array().unwrap()[0].as_array().unwrap().len();
            self.batch_sizes.lock().unwrap().push(signature_count);
            let statuses = vec![Some(self.status.clone()); signature_count];
            Ok(serde_json::to_value(Response {
                context: RpcResponseContext {
                    slot: 1,
                    api_version: None,
                },
                value: statuses,
            })
            .unwrap())
        }

        fn get_transport_stats(&self) -> RpcTransportStats {
            RpcTransportStats::default()
        }

        fn url(&self) -> String {
            "recording".to_string()
        }
    }
}
