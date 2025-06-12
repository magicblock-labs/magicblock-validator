use std::{collections::HashSet, path::Path};

use log::*;
use magicblock_committor_program::Changeset;
use solana_pubkey::Pubkey;
use solana_sdk::{hash::Hash, instruction::Instruction, signature::Keypair};
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
        /// Called once the pubkeys have been reserved
        respond_to: oneshot::Sender<CommittorServiceResult<()>>,
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
    RecommitChangeset {
        /// The request ID of the changeset to recommit
        reqid: String,
        /// Called once the changeset has been recommitted
        respond_to: oneshot::Sender<()>,
        /// The changeset to recommit
        changeset: Changeset,
        /// The blockhash in the ephemeral at the time the commit was requested
        ephemeral_blockhash: Hash,
        /// If `true`, account commits will be finalized after they were processed
        finalize: bool,
    },
    RefinalizeAccounts {
        /// Called once the accounts have been refinalized
        respond_to: oneshot::Sender<CommittorServiceResult<()>>,
        /// The request ID of the changeset to refinalize
        reqid: String,
        /// The accounts to finalize again since that step failed after they
        /// were committed previously
        /// The second element in the tuple indicates whether the account will
        /// need to be undelegated after finalization which means the retry is
        /// not complete until that step is done
        accounts: Vec<(Pubkey, bool)>,
    },
    ReundelegateAccounts {
        /// The request ID of the changeset to reundelegate
        reqid: String,
        /// Called once the accounts have been reundelegated
        respond_to: oneshot::Sender<CommittorServiceResult<()>>,
        /// The accounts to undelegate since that step failed after they
        /// were committed and possibly finalized previously
        accounts: Vec<Pubkey>,
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
    GetReqids {
        respond_to: oneshot::Sender<CommittorServiceResult<HashSet<String>>>,
    },
    RemoveCommitStatusesWithReqid {
        reqid: String,
        respond_to: oneshot::Sender<CommittorServiceResult<usize>>,
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
    processor: CommittorProcessor,
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
        let processor =
            CommittorProcessor::try_new(authority, persist_file, chain_config)?;
        Ok(Self {
            receiver,
            processor,
        })
    }

    async fn handle_msg(&self, msg: CommittorMessage) {
        use CommittorMessage::*;
        match msg {
            ReservePubkeysForCommittee {
                respond_to,
                committee,
                owner,
            } => {
                let pubkeys =
                    provide_committee_pubkeys(&committee, Some(&owner));
                let reqid = self.processor.reserve_pubkeys(pubkeys).await;
                if let Err(e) = respond_to.send(reqid) {
                    error!("Failed to send response {:?}", e);
                }
            }
            ReserveCommonPubkeys { respond_to } => {
                let pubkeys =
                    provide_common_pubkeys(&self.processor.auth_pubkey());
                let reqid = self.processor.reserve_pubkeys(pubkeys).await;
                if let Err(e) = respond_to.send(reqid) {
                    error!("Failed to send response {:?}", e);
                }
            }
            ReleaseCommonPubkeys { respond_to } => {
                let pubkeys =
                    provide_common_pubkeys(&self.processor.auth_pubkey());
                self.processor.release_pubkeys(pubkeys).await;
                if let Err(e) = respond_to.send(()) {
                    error!("Failed to send response {:?}", e);
                }
            }
            CommitChangeset {
                changeset,
                ephemeral_blockhash,
                respond_to,
                finalize,
            } => {
                let reqid = self
                    .processor
                    .commit_changeset(changeset, finalize, ephemeral_blockhash)
                    .await;
                if let Err(e) = respond_to.send(reqid) {
                    error!("Failed to send response {:?}", e);
                }
            }
            RecommitChangeset {
                reqid,
                changeset,
                ephemeral_blockhash,
                respond_to,
                finalize,
            } => {
                self.processor
                    .recommit_changeset(
                        &reqid,
                        changeset,
                        finalize,
                        ephemeral_blockhash,
                    )
                    .await;
                if let Err(e) = respond_to.send(()) {
                    error!("Failed to send response {:?}", e);
                }
            }
            RefinalizeAccounts {
                reqid,
                accounts,
                respond_to,
            } => {
                let res =
                    self.processor.refinalize_accounts(&reqid, accounts).await;
                if let Err(e) = respond_to.send(res) {
                    error!("Failed to send response {:?}", e);
                }
            }
            ReundelegateAccounts {
                reqid,
                respond_to,
                accounts,
            } => {
                let res = self
                    .processor
                    .reundelegate_accounts(&reqid, accounts)
                    .await;
                if let Err(e) = respond_to.send(res) {
                    error!("Failed to send response {:?}", e);
                }
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
            GetReqids { respond_to } => {
                let reqids = self.processor.get_reqids();
                if let Err(e) = respond_to.send(reqids) {
                    error!("Failed to send response {:?}", e);
                }
            }
            RemoveCommitStatusesWithReqid { reqid, respond_to } => {
                let remove_res =
                    self.processor.remove_commit_statuses_with_reqid(&reqid);
                if let Err(e) = respond_to.send(remove_res) {
                    error!("Failed to send response {:?}", e);
                }
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
    ) -> oneshot::Receiver<CommittorServiceResult<()>> {
        let (tx, rx) = oneshot::channel();
        self.try_send(CommittorMessage::ReservePubkeysForCommittee {
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

    fn recommit_changeset(
        &self,
        reqid: String,
        changeset: Changeset,
        ephemeral_blockhash: Hash,
        finalize: bool,
    ) -> oneshot::Receiver<()> {
        let (tx, rx) = oneshot::channel();
        self.try_send(CommittorMessage::RecommitChangeset {
            reqid,
            respond_to: tx,
            changeset,
            ephemeral_blockhash,
            finalize,
        });
        rx
    }

    fn refinalize_accounts(
        &self,
        reqid: String,
        accounts: Vec<(Pubkey, bool)>,
    ) -> oneshot::Receiver<CommittorServiceResult<()>> {
        let (tx, rx) = oneshot::channel();
        self.try_send(CommittorMessage::RefinalizeAccounts {
            respond_to: tx,
            reqid,
            accounts,
        });
        rx
    }

    fn reundelegate_accounts(
        &self,
        reqid: String,
        accounts: Vec<Pubkey>,
    ) -> oneshot::Receiver<CommittorServiceResult<()>> {
        let (tx, rx) = oneshot::channel();
        self.try_send(CommittorMessage::ReundelegateAccounts {
            respond_to: tx,
            reqid,
            accounts,
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

    fn get_reqids(
        &self,
    ) -> oneshot::Receiver<CommittorServiceResult<HashSet<String>>> {
        let (tx, rx) = oneshot::channel();
        self.try_send(CommittorMessage::GetReqids { respond_to: tx });
        rx
    }

    fn remove_commit_statuses_with_reqid(
        &self,
        reqid: String,
    ) -> oneshot::Receiver<CommittorServiceResult<usize>> {
        let (tx, rx) = oneshot::channel();
        self.try_send(CommittorMessage::RemoveCommitStatusesWithReqid {
            reqid,
            respond_to: tx,
        });
        rx
    }

    fn run_validator_signed_ixs(
        &self,
        _ixs: Vec<Instruction>,
    ) -> oneshot::Receiver<CommittorServiceResult<()>> {
        // TODO: @@@ run_validator_signed_ixs
        let (_tx, rx) = oneshot::channel();
        rx
    }
}

pub trait ChangesetCommittor: Send + Sync + 'static {
    /// Reserves pubkeys used in most commits in a lookup table
    fn reserve_pubkeys_for_committee(
        &self,
        committee: Pubkey,
        owner: Pubkey,
    ) -> oneshot::Receiver<CommittorServiceResult<()>>;

    /// Commits the changeset and returns the reqid
    fn commit_changeset(
        &self,
        changeset: Changeset,
        ephemeral_blockhash: Hash,
        finalize: bool,
    ) -> oneshot::Receiver<Option<String>>;

    /// Recommits the changeset with the provided reqid
    fn recommit_changeset(
        &self,
        reqid: String,
        changeset: Changeset,
        ephemeral_blockhash: Hash,
        finalize: bool,
    ) -> oneshot::Receiver<()>;

    /// Retries finalizing the accounts that were committed previously
    fn refinalize_accounts(
        &self,
        reqid: String,
        accounts: Vec<(Pubkey, bool)>,
    ) -> oneshot::Receiver<CommittorServiceResult<()>>;

    /// Retries undelegating the accounts
    fn reundelegate_accounts(
        &self,
        reqid: String,
        accounts: Vec<Pubkey>,
    ) -> oneshot::Receiver<CommittorServiceResult<()>>;

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

    /// Gets the list of all requests stored in the DB
    fn get_reqids(
        &self,
    ) -> oneshot::Receiver<CommittorServiceResult<HashSet<String>>>;

    /// Removes all commit statuses with the provided reqid from the database
    fn remove_commit_statuses_with_reqid(
        &self,
        reqid: String,
    ) -> oneshot::Receiver<CommittorServiceResult<usize>>;

    /// Chunks the provided instructions into as few transactions as possible then signs
    /// them with the validator authority and sends them.
    fn run_validator_signed_ixs(
        &self,
        ixs: Vec<Instruction>,
    ) -> oneshot::Receiver<CommittorServiceResult<()>>;
}
