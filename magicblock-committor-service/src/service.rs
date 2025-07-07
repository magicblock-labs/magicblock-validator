use std::{path::Path, sync::Arc, time::Instant};

use log::*;
use magicblock_committor_program::Changeset;
use solana_pubkey::Pubkey;
use solana_sdk::{hash::Hash, signature::Keypair};
use tokio::{
    select,
    sync::{
        mpsc::{self, error::TrySendError},
        oneshot,
    },
};
use tokio_util::sync::CancellationToken;

use crate::{
    commit::CommittorProcessor,
    config::ChainConfig,
    error::CommittorServiceResult,
    persist::{BundleSignatureRow, CommitStatusRow},
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
    CommitChangeset {
        /// Called once the changeset has been committed
        respond_to: oneshot::Sender<Option<String>>,
        /// The changeset to commit
        changeset: Changeset,
        /// The blockhash in the ephemeral at the time the commit was requested
        ephemeral_blockhash: Hash,
        /// If `true`, account commits will be finalized after they were processed
        finalize: bool,
    },
    GetCommitStatuses {
        respond_to:
            oneshot::Sender<CommittorServiceResult<Vec<CommitStatusRow>>>,
        reqid: String,
    },
    GetBundleSignatures {
        respond_to:
            oneshot::Sender<CommittorServiceResult<Option<BundleSignatureRow>>>,
        bundle_id: u64,
    },
    GetLookupTables {
        respond_to: oneshot::Sender<LookupTables>,
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
            CommitChangeset {
                changeset,
                ephemeral_blockhash,
                respond_to,
                finalize,
            } => {
                let processor = self.processor.clone();
                tokio::task::spawn(async move {
                    let reqid = processor
                        .commit_changeset(
                            changeset,
                            finalize,
                            ephemeral_blockhash,
                        )
                        .await;
                    if let Err(e) = respond_to.send(reqid) {
                        error!("Failed to send response {:?}", e);
                    }
                });
            }
            GetCommitStatuses { reqid, respond_to } => {
                let commit_statuses =
                    self.processor.get_commit_statuses(&reqid);
                if let Err(e) = respond_to.send(commit_statuses) {
                    error!("Failed to send response {:?}", e);
                }
            }
            GetBundleSignatures {
                bundle_id,
                respond_to,
            } => {
                let sig = self.processor.get_signature(bundle_id);
                if let Err(e) = respond_to.send(sig) {
                    error!("Failed to send response {:?}", e);
                }
            }
            GetLookupTables { respond_to } => {
                let processor = self.processor.clone();
                tokio::task::spawn(async move {
                    let active_tables = processor.active_lookup_tables().await;
                    let released_tables =
                        processor.released_lookup_tables().await;
                    if let Err(e) = respond_to.send(LookupTables {
                        active: active_tables,
                        released: released_tables,
                    }) {
                        error!("Failed to send response {:?}", e);
                    }
                });
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

    pub fn get_bundle_signatures(
        &self,
        bundle_id: u64,
    ) -> oneshot::Receiver<CommittorServiceResult<Option<BundleSignatureRow>>>
    {
        let (tx, rx) = oneshot::channel();
        self.try_send(CommittorMessage::GetBundleSignatures {
            respond_to: tx,
            bundle_id,
        });
        rx
    }

    pub fn get_lookup_tables(&self) -> oneshot::Receiver<LookupTables> {
        let (tx, rx) = oneshot::channel();
        self.try_send(CommittorMessage::GetLookupTables { respond_to: tx });
        rx
    }

    pub fn stop(&self) {
        self.cancel_token.cancel();
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

impl ChangesetCommittor for CommittorService {
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

    fn commit_changeset(
        &self,
        changeset: Changeset,
        ephemeral_blockhash: Hash,
        finalize: bool,
    ) -> oneshot::Receiver<Option<String>> {
        let (tx, rx) = oneshot::channel();
        self.try_send(CommittorMessage::CommitChangeset {
            respond_to: tx,
            changeset,
            ephemeral_blockhash,
            finalize,
        });
        rx
    }

    fn get_commit_statuses(
        &self,
        reqid: String,
    ) -> oneshot::Receiver<CommittorServiceResult<Vec<CommitStatusRow>>> {
        let (tx, rx) = oneshot::channel();
        self.try_send(CommittorMessage::GetCommitStatuses {
            respond_to: tx,
            reqid,
        });
        rx
    }

    fn get_bundle_signatures(
        &self,
        bundle_id: u64,
    ) -> oneshot::Receiver<CommittorServiceResult<Option<BundleSignatureRow>>>
    {
        let (tx, rx) = oneshot::channel();
        self.try_send(CommittorMessage::GetBundleSignatures {
            respond_to: tx,
            bundle_id,
        });
        rx
    }
}

pub trait ChangesetCommittor: Send + Sync + 'static {
    /// Reserves pubkeys used in most commits in a lookup table
    fn reserve_pubkeys_for_committee(
        &self,
        committee: Pubkey,
        owner: Pubkey,
    ) -> oneshot::Receiver<CommittorServiceResult<Instant>>;

    /// Commits the changeset and returns the reqid
    fn commit_changeset(
        &self,
        changeset: Changeset,
        ephemeral_blockhash: Hash,
        finalize: bool,
    ) -> oneshot::Receiver<Option<String>>;

    /// Gets statuses of accounts that were committed as part of a request with provided reqid
    fn get_commit_statuses(
        &self,
        reqid: String,
    ) -> oneshot::Receiver<CommittorServiceResult<Vec<CommitStatusRow>>>;

    /// Gets signatures of commits processed as part of the bundle with the provided bundle_id
    fn get_bundle_signatures(
        &self,
        bundle_id: u64,
    ) -> oneshot::Receiver<CommittorServiceResult<Option<BundleSignatureRow>>>;
}
