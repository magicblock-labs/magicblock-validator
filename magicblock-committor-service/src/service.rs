use std::{path::Path, sync::Arc, time::Instant};

use log::*;
use solana_pubkey::Pubkey;
use solana_sdk::signature::{Keypair, Signature};
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

use crate::{
    committor_processor::CommittorProcessor,
    config::ChainConfig,
    error::CommittorServiceResult,
    intent_execution_manager::BroadcastedIntentExecutionResult,
    persist::{CommitStatusRow, MessageSignatures},
    pubkeys_provider::{provide_committee_pubkeys, provide_common_pubkeys},
    types::ScheduledBaseIntentWrapper,
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
    ScheduleBaseIntents {
        /// The [`ScheduledBaseIntent`]s to commit
        base_intents: Vec<ScheduledBaseIntentWrapper>,
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
}

// -----------------
// CommittorActor
// -----------------
struct CommittorActor {
    receiver: mpsc::Receiver<CommittorMessage>,
    processor: Arc<CommittorProcessor>,
}

impl CommittorActor {
    pub fn try_new<P>(
        receiver: mpsc::Receiver<CommittorMessage>,
        authority: Keypair,
        persist_file: P,
        chain_config: ChainConfig,
    ) -> CommittorServiceResult<Self>
    where
        P: AsRef<Path>,
    {
        let processor = Arc::new(CommittorProcessor::try_new(
            authority,
            persist_file,
            chain_config,
        )?);

        Ok(Self {
            receiver,
            processor,
        })
    }

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
                        error!("Failed to send response {:?}", e);
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
                        error!("Failed to send response {:?}", e);
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
                        error!("Failed to send response {:?}", e);
                    }
                });
            }
            ScheduleBaseIntents {
                base_intents,
                respond_to,
            } => {
                let result =
                    self.processor.schedule_base_intents(base_intents).await;
                if let Err(e) = respond_to.send(result) {
                    error!("Failed to send response {:?}", e);
                }
            }
            GetCommitStatuses {
                message_id,
                respond_to,
            } => {
                let commit_statuses =
                    self.processor.get_commit_statuses(message_id);
                if let Err(e) = respond_to.send(commit_statuses) {
                    error!("Failed to send response {:?}", e);
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
                    error!("Failed to send response {:?}", e);
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
                        error!( "Failed to send response for GetTransactionLogs: {:?}", err);
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
                    error!("Failed to send response {:?}", e);
                }
            }
            SubscribeForResults { respond_to } => {
                let subscription = self.processor.subscribe_for_results();
                if let Err(err) = respond_to.send(subscription) {
                    error!("Failed to send response {:?}", err);
                }
            }
        }
    }

    pub async fn run(&mut self, cancel_token: CancellationToken) {
        loop {
            select! {
                msg = self.receiver.recv() => {
                    if let Some(msg) = msg {
                        self.handle_msg(msg).await;
                    } else {
                        break;
                    }
                }
                _ = cancel_token.cancelled() => {
                    break;
                }
            }
        }
    }
}

// -----------------
// CommittorService
// -----------------
pub struct CommittorService {
    sender: mpsc::Sender<CommittorMessage>,
    cancel_token: CancellationToken,
}

impl CommittorService {
    pub fn try_start<P>(
        authority: Keypair,
        persist_file: P,
        chain_config: ChainConfig,
    ) -> CommittorServiceResult<Self>
    where
        P: AsRef<Path>,
    {
        debug!("Starting committor service with config: {:?}", chain_config);
        let (sender, receiver) = mpsc::channel(1_000);
        let cancel_token = CancellationToken::new();
        {
            let cancel_token = cancel_token.clone();
            let mut actor = CommittorActor::try_new(
                receiver,
                authority,
                persist_file,
                chain_config,
            )?;
            tokio::spawn(async move {
                actor.run(cancel_token).await;
            });
        }
        Ok(Self {
            sender,
            cancel_token,
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

    fn schedule_base_intent(
        &self,
        base_intents: Vec<ScheduledBaseIntentWrapper>,
    ) -> oneshot::Receiver<CommittorServiceResult<()>> {
        let (tx, rx) = oneshot::channel();
        self.try_send(CommittorMessage::ScheduleBaseIntents {
            base_intents,
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

    fn stop(&self) {
        self.cancel_token.cancel();
    }

    fn stopped(&self) -> WaitForCancellationFutureOwned {
        self.cancel_token.clone().cancelled_owned()
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
    fn schedule_base_intent(
        &self,
        base_intents: Vec<ScheduledBaseIntentWrapper>,
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

    /// Stops Committor service
    fn stop(&self);

    /// Returns future which resolves once committor `stop` got called
    fn stopped(&self) -> WaitForCancellationFutureOwned;
}
