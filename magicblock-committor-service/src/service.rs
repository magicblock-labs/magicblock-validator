use std::{
    collections::HashMap,
    path::Path,
    sync::{atomic::AtomicU64, Arc},
    time::Instant,
};

use magicblock_core::traits::ActionsCallbackScheduler;
use magicblock_program::magic_scheduled_base_intent::ScheduledIntentBundle;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_transaction_status_client_types::EncodedConfirmedTransactionWithStatusMeta;
use tokio::{
    select,
    sync::{
        broadcast,
        mpsc::{self, error::TrySendError},
        oneshot,
    },
};
use tokio_util::sync::{CancellationToken, WaitForCancellationFutureOwned};
use tracing::*;

use crate::{
    committor_processor::CommittorProcessor,
    config::ChainConfig,
    error::{CommittorServiceError, CommittorServiceResult},
    intent_execution_manager::BroadcastedIntentExecutionResult,
    persist::{CommitStatusRow, MessageSignatures},
    pubkeys_provider::{provide_committee_pubkeys, provide_common_pubkeys},
};

#[derive(Debug)]
pub struct LookupTables {
    pub active: Vec<Pubkey>,
    pub released: Vec<Pubkey>,
}

#[derive(Debug)]
pub enum CommittorMessage {
    ReservePubkeysForCommittee {
        /// When the request was initiated
        initiated: Instant,
        /// Called once the pubkeys have been reserved and includes that timestamp
        /// at which the request was initiated
        respond_to: oneshot::Sender<CommittorServiceResult<Instant>>,
        /// The committee whose pubkeys to reserve in a lookup table
        /// These pubkeys are used to process/finalize the commit
        committee: Pubkey,
        /// The owner program of the committee
        owner: Pubkey,
    },
    ReserveCommonPubkeys {
        /// Called once the pubkeys have been reserved
        respond_to: oneshot::Sender<CommittorServiceResult<()>>,
    },
    ReleaseCommonPubkeys {
        /// Called once the pubkeys have been released
        respond_to: oneshot::Sender<()>,
    },
    ScheduleIntentBundle {
        /// The [`ScheduleIntentBundle`]s to commit
        intent_bundles: Vec<ScheduledIntentBundle>,
        respond_to: oneshot::Sender<CommittorServiceResult<()>>,
    },
    GetPendingIntentBundles {
        respond_to:
            oneshot::Sender<CommittorServiceResult<Vec<ScheduledIntentBundle>>>,
    },
    ScheduleRecoveredIntentBundle {
        /// Recovered [`ScheduleIntentBundle`]s to commit without re-persisting rows.
        intent_bundles: Vec<ScheduledIntentBundle>,
        respond_to: oneshot::Sender<CommittorServiceResult<()>>,
    },
    GetCommitStatuses {
        respond_to:
            oneshot::Sender<CommittorServiceResult<Vec<CommitStatusRow>>>,
        message_id: u64,
    },
    GetCommitSignatures {
        respond_to:
            oneshot::Sender<CommittorServiceResult<Option<MessageSignatures>>>,
        commit_id: u64,
        pubkey: Pubkey,
    },
    GetLookupTables {
        respond_to: oneshot::Sender<LookupTables>,
    },
    GetTransaction {
        respond_to: oneshot::Sender<
            CommittorServiceResult<EncodedConfirmedTransactionWithStatusMeta>,
        >,
        signature: Signature,
    },
    SubscribeForResults {
        respond_to: oneshot::Sender<
            broadcast::Receiver<BroadcastedIntentExecutionResult>,
        >,
    },
    FetchCurrentCommitNonces {
        respond_to:
            oneshot::Sender<CommittorServiceResult<HashMap<Pubkey, u64>>>,
        pubkeys: Vec<Pubkey>,
        min_context_slot: u64,
    },
    FetchCurrentCommitNoncesSync {
        respond_to: std::sync::mpsc::Sender<
            CommittorServiceResult<HashMap<Pubkey, u64>>,
        >,
        pubkeys: Vec<Pubkey>,
        min_context_slot: u64,
    },
}

// -----------------
// CommittorActor
// -----------------
struct CommittorActor {
    receiver: mpsc::Receiver<CommittorMessage>,
    processor: Arc<CommittorProcessor>,
}

impl CommittorActor {
    pub fn try_new<P, A>(
        receiver: mpsc::Receiver<CommittorMessage>,
        authority: Keypair,
        persist_file: P,
        chain_config: ChainConfig,
        chain_slot: Option<Arc<AtomicU64>>,
        actions_callback_executor: A,
    ) -> CommittorServiceResult<Self>
    where
        P: AsRef<Path>,
        A: ActionsCallbackScheduler,
    {
        let processor = Arc::new(CommittorProcessor::try_new(
            authority,
            persist_file,
            chain_config,
            chain_slot,
            actions_callback_executor,
        )?);

        Ok(Self {
            receiver,
            processor,
        })
    }

    #[instrument(skip(self))]
    async fn handle_msg(&self, msg: CommittorMessage) {
        use CommittorMessage::*;
        match msg {
            ReservePubkeysForCommittee {
                initiated,
                respond_to,
                committee,
                owner,
            } => {
                let processor = self.processor.clone();
                tokio::task::spawn(async move {
                    let pubkeys =
                        provide_committee_pubkeys(&committee, Some(&owner));
                    // NOTE: we wait here until the reservation is done which causes the
                    // cloning of a particular account to be blocked.
                    // This leads to larger delays on the first clone of an account, but also
                    // ensures that the account could be committed via a lookup table later.
                    let result = processor
                        .reserve_pubkeys(pubkeys)
                        .await
                        .map(|_| initiated);
                    if let Err(e) = respond_to.send(result) {
                        error!(message_type = "ReservePubkeysForCommittee", error = ?e, "Failed to send response");
                    }
                });
            }
            ReserveCommonPubkeys { respond_to } => {
                let processor = self.processor.clone();
                tokio::task::spawn(async move {
                    let pubkeys =
                        provide_common_pubkeys(&processor.auth_pubkey());
                    let reqid = processor.reserve_pubkeys(pubkeys).await;
                    if let Err(e) = respond_to.send(reqid) {
                        error!(message_type = "ReserveCommonPubkeys", error = ?e, "Failed to send response");
                    }
                });
            }
            ReleaseCommonPubkeys { respond_to } => {
                let processor = self.processor.clone();
                tokio::task::spawn(async move {
                    let pubkeys =
                        provide_common_pubkeys(&processor.auth_pubkey());
                    processor.release_pubkeys(pubkeys).await;
                    if let Err(e) = respond_to.send(()) {
                        error!(message_type = "ReleaseCommonPubkeys", error = ?e, "Failed to send response");
                    }
                });
            }
            ScheduleIntentBundle {
                intent_bundles,
                respond_to,
            } => {
                let result =
                    self.processor.schedule_intent_bundle(intent_bundles).await;
                if let Err(e) = respond_to.send(result) {
                    error!(message_type = "ScheduleBaseIntents", error = ?e, "Failed to send response");
                }
            }
            GetPendingIntentBundles { respond_to } => {
                let pending_intents =
                    self.processor.pending_intent_bundles().await;
                if let Err(e) = respond_to.send(pending_intents) {
                    error!(message_type = "GetPendingIntentBundles", error = ?e, "Failed to send response");
                }
            }
            ScheduleRecoveredIntentBundle {
                intent_bundles,
                respond_to,
            } => {
                let result = self
                    .processor
                    .schedule_recovered_intent_bundles(intent_bundles)
                    .await;
                if let Err(e) = respond_to.send(result) {
                    error!(message_type = "ScheduleRecoveredIntentBundle", error = ?e, "Failed to send response");
                }
            }
            GetCommitStatuses {
                message_id,
                respond_to,
            } => {
                let commit_statuses =
                    self.processor.get_commit_statuses(message_id);
                if let Err(e) = respond_to.send(commit_statuses) {
                    error!(message_type = "GetCommitStatuses", error = ?e, "Failed to send response");
                }
            }
            GetCommitSignatures {
                commit_id,
                respond_to,
                pubkey,
            } => {
                let sig =
                    self.processor.get_commit_signature(commit_id, pubkey);
                if let Err(e) = respond_to.send(sig) {
                    error!(message_type = "GetCommitSignatures", error = ?e, "Failed to send response");
                }
            }
            GetTransaction {
                signature,
                respond_to,
            } => {
                let processor = self.processor.clone();
                tokio::task::spawn(async move {
                    let res = processor
                        .magicblock_rpc_client
                        .get_transaction(&signature, None)
                        .await
                        .map_err(Into::into);
                    if let Err(err) = respond_to.send(res) {
                        error!(message_type = "GetTransaction", error = ?err, "Failed to send response");
                    }
                });
            }
            GetLookupTables { respond_to } => {
                let active_tables = self.processor.active_lookup_tables().await;
                let released_tables =
                    self.processor.released_lookup_tables().await;
                if let Err(e) = respond_to.send(LookupTables {
                    active: active_tables,
                    released: released_tables,
                }) {
                    error!(message_type = "GetLookupTables", error = ?e, "Failed to send response");
                }
            }
            SubscribeForResults { respond_to } => {
                let subscription = self.processor.subscribe_for_results();
                if let Err(err) = respond_to.send(subscription) {
                    error!(message_type = "SubscribeForResults", error = ?err, "Failed to send response");
                }
            }
            FetchCurrentCommitNonces {
                respond_to,
                pubkeys,
                min_context_slot,
            } => {
                let processor = self.processor.clone();
                tokio::spawn(async move {
                    let result = processor
                        .fetch_current_commit_nonces(&pubkeys, min_context_slot)
                        .await;
                    if let Err(err) = respond_to
                        .send(result.map_err(CommittorServiceError::from))
                    {
                        error!(message_type = "FetchCurrentCommitNonces", error = ?err, "Failed to send response");
                    }
                });
            }
            FetchCurrentCommitNoncesSync {
                respond_to,
                pubkeys,
                min_context_slot,
            } => {
                let processor = self.processor.clone();
                tokio::spawn(async move {
                    let result = processor
                        .fetch_current_commit_nonces(&pubkeys, min_context_slot)
                        .await;
                    if let Err(err) = respond_to
                        .send(result.map_err(CommittorServiceError::from))
                    {
                        error!(message_type = "FetchCurrentCommitNoncesSync", error = ?err, "Failed to send response");
                    }
                });
            }
        }
    }

    fn reject_msg(msg: CommittorMessage) {
        use CommittorMessage::*;
        fn shutdown_err<T>(
            respond_to: oneshot::Sender<Result<T, CommittorServiceError>>,
        ) {
            let _ = respond_to.send(Err(CommittorServiceError::ShuttingDown));
        }

        match msg {
            ReservePubkeysForCommittee { respond_to, .. } => {
                shutdown_err(respond_to)
            }
            ReserveCommonPubkeys { respond_to } => shutdown_err(respond_to),
            ScheduleIntentBundle { respond_to, .. } => shutdown_err(respond_to),
            GetPendingIntentBundles { respond_to } => shutdown_err(respond_to),
            ScheduleRecoveredIntentBundle { respond_to, .. } => {
                shutdown_err(respond_to)
            }
            GetCommitStatuses { respond_to, .. } => shutdown_err(respond_to),
            GetCommitSignatures { respond_to, .. } => shutdown_err(respond_to),
            GetTransaction { respond_to, .. } => shutdown_err(respond_to),
            FetchCurrentCommitNonces { respond_to, .. } => {
                shutdown_err(respond_to)
            }
            FetchCurrentCommitNoncesSync { respond_to, .. } => {
                let _ =
                    respond_to.send(Err(CommittorServiceError::ShuttingDown));
            }

            ReleaseCommonPubkeys { respond_to } => {
                let _ = respond_to.send(());
            }
            GetLookupTables { respond_to } => {
                let _ = respond_to.send(LookupTables {
                    active: Vec::new(),
                    released: Vec::new(),
                });
            }
            SubscribeForResults { respond_to } => {
                let (_tx, rx) = broadcast::channel(1);
                let _ = respond_to.send(rx);
            }
        }
    }

    fn drain_rejected_messages(
        receiver: &mut mpsc::Receiver<CommittorMessage>,
    ) {
        while let Ok(msg) = receiver.try_recv() {
            Self::reject_msg(msg);
        }
    }

    #[instrument(skip(self, shutdown_token))]
    pub async fn run(&mut self, shutdown_token: CancellationToken) {
        let mut shutting_down = false;

        loop {
            if shutting_down {
                break;
            }

            select! {
                biased;
                _ = shutdown_token.cancelled(), if !shutting_down => {
                    shutting_down = true;
                    info!("CommittorActor shutdown initiated");
                    Self::drain_rejected_messages(&mut self.receiver);
                }
                msg = self.receiver.recv() => {
                    match msg {
                        Some(msg) if !shutting_down => {
                            self.handle_msg(msg).await;
                        }
                        Some(msg) => {
                            Self::reject_msg(msg);
                        }
                        None => break,
                    }
                }
            }
        }

        self.processor.shutdown().await;
        info!("CommittorActor shutdown complete");
    }
}

// -----------------
// CommittorService
// -----------------
pub struct CommittorService {
    sender: mpsc::Sender<CommittorMessage>,
    shutdown_token: CancellationToken,
    actor_done_token: CancellationToken,
}

impl CommittorService {
    pub fn try_start<P, A>(
        authority: Keypair,
        persist_file: P,
        chain_config: ChainConfig,
        chain_slot: Option<Arc<AtomicU64>>,
        actions_callback_executor: A,
    ) -> CommittorServiceResult<Self>
    where
        P: AsRef<Path>,
        A: ActionsCallbackScheduler,
    {
        debug!("Starting committor service");
        let (sender, receiver) = mpsc::channel(1_000);
        let shutdown_token = CancellationToken::new();
        let actor_done_token = CancellationToken::new();
        {
            let shutdown_token = shutdown_token.clone();
            let actor_done_token = actor_done_token.clone();
            let mut actor = CommittorActor::try_new(
                receiver,
                authority,
                persist_file,
                chain_config,
                chain_slot,
                actions_callback_executor,
            )?;
            tokio::spawn(async move {
                actor.run(shutdown_token).await;
                actor_done_token.cancel();
            });
        }
        Ok(Self {
            sender,
            shutdown_token,
            actor_done_token,
        })
    }

    pub fn reserve_common_pubkeys(
        &self,
    ) -> oneshot::Receiver<CommittorServiceResult<()>> {
        let (tx, rx) = oneshot::channel();
        self.try_send(CommittorMessage::ReserveCommonPubkeys {
            respond_to: tx,
        });
        rx
    }

    pub fn release_common_pubkeys(&self) -> oneshot::Receiver<()> {
        let (tx, rx) = oneshot::channel();
        self.try_send(CommittorMessage::ReleaseCommonPubkeys {
            respond_to: tx,
        });
        rx
    }

    pub fn get_pending_intent_bundles(
        &self,
    ) -> oneshot::Receiver<CommittorServiceResult<Vec<ScheduledIntentBundle>>>
    {
        let (tx, rx) = oneshot::channel();
        self.try_send(CommittorMessage::GetPendingIntentBundles {
            respond_to: tx,
        });
        rx
    }

    pub fn schedule_recovered_intent_bundles(
        &self,
        intent_bundles: Vec<ScheduledIntentBundle>,
    ) -> oneshot::Receiver<CommittorServiceResult<()>> {
        let (tx, rx) = oneshot::channel();
        self.try_send(CommittorMessage::ScheduleRecoveredIntentBundle {
            intent_bundles,
            respond_to: tx,
        });
        rx
    }

    pub fn get_commit_signatures(
        &self,
        commit_id: u64,
        pubkey: Pubkey,
    ) -> oneshot::Receiver<CommittorServiceResult<Option<MessageSignatures>>>
    {
        let (tx, rx) = oneshot::channel();
        self.try_send(CommittorMessage::GetCommitSignatures {
            respond_to: tx,
            commit_id,
            pubkey,
        });
        rx
    }

    pub fn get_lookup_tables(&self) -> oneshot::Receiver<LookupTables> {
        let (tx, rx) = oneshot::channel();
        self.try_send(CommittorMessage::GetLookupTables { respond_to: tx });
        rx
    }

    pub fn fetch_current_commit_nonces_sync(
        &self,
        pubkeys: &[Pubkey],
        min_context_slot: u64,
    ) -> std::sync::mpsc::Receiver<CommittorServiceResult<HashMap<Pubkey, u64>>>
    {
        let (tx, rx) = std::sync::mpsc::channel();
        self.try_send(CommittorMessage::FetchCurrentCommitNoncesSync {
            respond_to: tx,
            pubkeys: pubkeys.to_vec(),
            min_context_slot,
        });
        rx
    }

    fn try_send(&self, msg: CommittorMessage) {
        if let Err(e) = self.sender.try_send(msg) {
            match e {
                TrySendError::Full(msg) => error!(
                    "Channel full, failed to send commit message {:?}",
                    msg
                ),
                TrySendError::Closed(msg) => error!(
                    "Channel closed, failed to send commit message {:?}",
                    msg
                ),
            }
        }
    }
}

impl BaseIntentCommittor for CommittorService {
    fn reserve_pubkeys_for_committee(
        &self,
        committee: Pubkey,
        owner: Pubkey,
    ) -> oneshot::Receiver<CommittorServiceResult<Instant>> {
        let (tx, rx) = oneshot::channel();
        self.try_send(CommittorMessage::ReservePubkeysForCommittee {
            initiated: Instant::now(),
            respond_to: tx,
            committee,
            owner,
        });
        rx
    }

    fn schedule_intent_bundles(
        &self,
        intent_bundles: Vec<ScheduledIntentBundle>,
    ) -> oneshot::Receiver<CommittorServiceResult<()>> {
        let (tx, rx) = oneshot::channel();
        self.try_send(CommittorMessage::ScheduleIntentBundle {
            intent_bundles,
            respond_to: tx,
        });
        rx
    }

    fn get_commit_statuses(
        &self,
        message_id: u64,
    ) -> oneshot::Receiver<CommittorServiceResult<Vec<CommitStatusRow>>> {
        let (tx, rx) = oneshot::channel();
        self.try_send(CommittorMessage::GetCommitStatuses {
            respond_to: tx,
            message_id,
        });
        rx
    }

    fn get_commit_signatures(
        &self,
        commit_id: u64,
        pubkey: Pubkey,
    ) -> oneshot::Receiver<CommittorServiceResult<Option<MessageSignatures>>>
    {
        let (tx, rx) = oneshot::channel();
        self.try_send(CommittorMessage::GetCommitSignatures {
            respond_to: tx,
            commit_id,
            pubkey,
        });
        rx
    }

    fn subscribe_for_results(
        &self,
    ) -> oneshot::Receiver<broadcast::Receiver<BroadcastedIntentExecutionResult>>
    {
        let (tx, rx) = oneshot::channel();
        self.try_send(CommittorMessage::SubscribeForResults { respond_to: tx });
        rx
    }

    fn get_transaction(
        &self,
        signature: &Signature,
    ) -> oneshot::Receiver<
        CommittorServiceResult<EncodedConfirmedTransactionWithStatusMeta>,
    > {
        let (tx, rx) = oneshot::channel();
        self.try_send(CommittorMessage::GetTransaction {
            respond_to: tx,
            signature: *signature,
        });

        rx
    }

    fn fetch_current_commit_nonces(
        &self,
        pubkeys: &[Pubkey],
        min_context_slot: u64,
    ) -> oneshot::Receiver<CommittorServiceResult<HashMap<Pubkey, u64>>> {
        let (tx, rx) = oneshot::channel();
        self.try_send(CommittorMessage::FetchCurrentCommitNonces {
            respond_to: tx,
            pubkeys: pubkeys.to_vec(),
            min_context_slot,
        });

        rx
    }

    fn stop(&self) {
        self.shutdown_token.cancel();
    }

    fn stopped(&self) -> WaitForCancellationFutureOwned {
        self.actor_done_token.clone().cancelled_owned()
    }
}

pub trait BaseIntentCommittor: Send + Sync + 'static {
    /// Reserves pubkeys used in most commits in a lookup table
    fn reserve_pubkeys_for_committee(
        &self,
        committee: Pubkey,
        owner: Pubkey,
    ) -> oneshot::Receiver<CommittorServiceResult<Instant>>;

    /// Commits the changeset and returns
    fn schedule_intent_bundles(
        &self,
        intent_bundles: Vec<ScheduledIntentBundle>,
    ) -> oneshot::Receiver<CommittorServiceResult<()>>;

    /// Subscribes for results of BaseIntent execution
    fn subscribe_for_results(
        &self,
    ) -> oneshot::Receiver<broadcast::Receiver<BroadcastedIntentExecutionResult>>;

    /// Gets statuses of accounts that were committed as part of a request with provided message_id
    fn get_commit_statuses(
        &self,
        message_id: u64,
    ) -> oneshot::Receiver<CommittorServiceResult<Vec<CommitStatusRow>>>;

    /// Gets signatures for commit of particular accounts
    fn get_commit_signatures(
        &self,
        commit_id: u64,
        pubkey: Pubkey,
    ) -> oneshot::Receiver<CommittorServiceResult<Option<MessageSignatures>>>;

    fn get_transaction(
        &self,
        signature: &Signature,
    ) -> oneshot::Receiver<
        CommittorServiceResult<EncodedConfirmedTransactionWithStatusMeta>,
    >;

    fn fetch_current_commit_nonces(
        &self,
        pubkeys: &[Pubkey],
        min_context_slot: u64,
    ) -> oneshot::Receiver<CommittorServiceResult<HashMap<Pubkey, u64>>>;

    /// Stops Committor service
    fn stop(&self);

    /// Returns future which resolves once the committor actor has finished
    /// draining in-flight intents and exited
    fn stopped(&self) -> WaitForCancellationFutureOwned;
}

#[cfg(test)]
mod shutdown_tests {
    use std::{sync::Arc, time::Duration};

    use solana_pubkey::pubkey;

    use super::*;
    use crate::{
        intent_execution_manager::{
            intent_scheduler::create_test_intent,
            test_support::MockIntentExecutorFactory,
        },
        test_utils,
    };

    fn try_start_for_test(
        factory: MockIntentExecutorFactory,
    ) -> Arc<CommittorService> {
        test_utils::init_test_logger();

        let (sender, receiver) = mpsc::channel(1000);
        let processor = Arc::new(CommittorProcessor::new_for_test(factory));
        let shutdown_token = CancellationToken::new();
        let actor_done_token = CancellationToken::new();
        let mut actor = CommittorActor {
            receiver,
            processor,
        };
        tokio::spawn({
            let shutdown_token = shutdown_token.clone();
            let actor_done_token = actor_done_token.clone();
            async move {
                actor.run(shutdown_token).await;
                actor_done_token.cancel();
            }
        });

        Arc::new(CommittorService {
            sender,
            shutdown_token,
            actor_done_token,
        })
    }

    #[test]
    fn reject_msg_responds_with_shutting_down_for_schedule() {
        let (respond_to, mut response) = oneshot::channel();
        CommittorActor::reject_msg(CommittorMessage::ScheduleIntentBundle {
            intent_bundles: Vec::new(),
            respond_to,
        });

        let err = response.try_recv().unwrap().unwrap_err();
        assert!(matches!(err, CommittorServiceError::ShuttingDown));
    }

    #[test]
    fn reject_msg_returns_empty_lookup_tables() {
        let (respond_to, mut response) = oneshot::channel();
        CommittorActor::reject_msg(CommittorMessage::GetLookupTables {
            respond_to,
        });

        let tables = response.try_recv().unwrap();
        assert!(tables.active.is_empty());
        assert!(tables.released.is_empty());
    }

    #[tokio::test]
    async fn test_stopped_waits_for_in_flight_intent() {
        let service = try_start_for_test(
            MockIntentExecutorFactory::new()
                .with_execution_delay(Duration::from_millis(200)),
        );
        let mut result_receiver =
            service.subscribe_for_results().await.unwrap();

        let intent = create_test_intent(
            1,
            &[pubkey!("1111111111111111111111111111111111111111111")],
            false,
        );
        service
            .schedule_intent_bundles(vec![intent])
            .await
            .unwrap()
            .unwrap();

        let stopped_task = {
            let service = service.clone();
            tokio::spawn(async move { service.stopped().await })
        };

        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            !stopped_task.is_finished(),
            "stopped() must not complete before shutdown is requested"
        );

        service.stop();

        let result = tokio::time::timeout(
            Duration::from_secs(5),
            result_receiver.recv(),
        )
        .await
        .expect("timed out waiting for in-flight intent result")
        .expect("result channel closed");
        assert!(result.is_ok());
        assert_eq!(result.id, 1);

        tokio::time::timeout(Duration::from_secs(5), stopped_task)
            .await
            .expect("timed out waiting for actor shutdown")
            .expect("stopped task panicked");
    }

    #[tokio::test]
    async fn test_queued_schedule_rejected_when_shutdown_drains_channel() {
        let service = try_start_for_test(
            MockIntentExecutorFactory::new()
                .with_execution_delay(Duration::from_millis(300)),
        );

        let blocking_intent = create_test_intent(
            1,
            &[pubkey!("1111111111111111111111111111111111111111111")],
            false,
        );
        let queued_intent = create_test_intent(
            2,
            &[pubkey!("22222222222222222222222222222222222222222222")],
            false,
        );

        service
            .schedule_intent_bundles(vec![blocking_intent])
            .await
            .unwrap()
            .unwrap();

        let queued_schedule =
            service.schedule_intent_bundles(vec![queued_intent]);
        service.stop();

        let err = tokio::time::timeout(Duration::from_secs(5), queued_schedule)
            .await
            .expect("timed out waiting for queued schedule response")
            .unwrap()
            .unwrap_err();
        assert!(matches!(err, CommittorServiceError::ShuttingDown));

        tokio::time::timeout(Duration::from_secs(5), service.stopped())
            .await
            .expect("timed out waiting for actor shutdown");
    }

    #[tokio::test]
    async fn test_service_ext_waits_for_in_flight_result_during_shutdown() {
        use crate::service_ext::{BaseIntentCommittorExt, CommittorServiceExt};

        let service = try_start_for_test(
            MockIntentExecutorFactory::new()
                .with_execution_delay(Duration::from_millis(200)),
        );
        let ext = CommittorServiceExt::new(service.clone());

        let intent = create_test_intent(
            1,
            &[pubkey!("1111111111111111111111111111111111111111111")],
            false,
        );
        let waiting = tokio::spawn(async move {
            ext.schedule_intent_bundles_waiting(vec![intent]).await
        });

        tokio::time::sleep(Duration::from_millis(20)).await;
        service.stop();

        let results = tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .expect("timed out waiting for in-flight result via CommittorServiceExt")
            .expect("waiting task panicked")
            .expect("schedule_intent_bundles_waiting failed");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, 1);
        assert!(results[0].inner.is_ok());

        tokio::time::timeout(Duration::from_secs(5), service.stopped())
            .await
            .expect("timed out waiting for actor shutdown");
    }
}
