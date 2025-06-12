use log::*;
use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use magicblock_committor_program::Changeset;
use solana_pubkey::Pubkey;
use solana_sdk::{hash::Hash, instruction::Instruction, signature::Signature};
use tokio::sync::oneshot;

use crate::{
    error::CommittorServiceResult,
    persist::{
        BundleSignatureRow, CommitStatus, CommitStatusRow,
        CommitStatusSignatures, CommitStrategy, CommitType,
    },
    ChangesetCommittor,
};

#[derive(Default)]
pub struct ChangesetCommittorStub {
    reserved_pubkeys_for_committee: Arc<Mutex<HashMap<Pubkey, Pubkey>>>,
    #[allow(clippy::type_complexity)]
    committed_changesets: Arc<Mutex<HashMap<u64, (Changeset, Hash, bool)>>>,
    #[allow(clippy::type_complexity)]
    recommitted_changesets:
        Arc<Mutex<HashMap<String, (Changeset, Hash, bool)>>>,
    commit_statuses: Arc<Mutex<HashMap<u64, Vec<CommitStatusRow>>>>,
    validator_signed_ixs: Arc<Mutex<Vec<Instruction>>>,
}

impl ChangesetCommittorStub {
    pub fn add_commit_status(&self, status_row: CommitStatusRow) {
        let reqid = status_row.reqid.parse::<u64>().unwrap();
        self.commit_statuses
            .lock()
            .unwrap()
            .entry(reqid)
            .or_default()
            .push(status_row);
    }

    pub fn committed_changesets(
        &self,
    ) -> HashMap<u64, (Changeset, Hash, bool)> {
        self.committed_changesets.lock().unwrap().clone()
    }

    pub fn recommitted_changesets(
        &self,
    ) -> HashMap<String, (Changeset, Hash, bool)> {
        self.recommitted_changesets.lock().unwrap().clone()
    }

    pub fn commit_statuses(&self) -> HashMap<u64, Vec<CommitStatusRow>> {
        self.commit_statuses.lock().unwrap().clone()
    }

    pub fn validator_signed_ixs(&self) -> Vec<Instruction> {
        self.validator_signed_ixs.lock().unwrap().clone()
    }
}

impl ChangesetCommittor for ChangesetCommittorStub {
    fn commit_changeset(
        &self,
        changeset: Changeset,
        ephemeral_blockhash: Hash,
        finalize: bool,
    ) -> oneshot::Receiver<Option<String>> {
        static REQ_ID: AtomicU64 = AtomicU64::new(0);
        let reqid = REQ_ID.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.committed_changesets
            .lock()
            .unwrap()
            .insert(reqid, (changeset, ephemeral_blockhash, finalize));
        tx.send(Some(reqid.to_string())).unwrap_or_else(|_| {
            error!("Failed to send commit changeset response");
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
        let (tx, rx) = tokio::sync::oneshot::channel();
        debug!(
            "Recommitting changeset for reqid: {} {:?}",
            reqid, changeset
        );
        self.recommitted_changesets.lock().unwrap().insert(
            reqid.to_string(),
            (changeset, ephemeral_blockhash, finalize),
        );
        tx.send(()).unwrap_or_else(|_| {
            error!("Failed to send recommit changeset response");
        });
        rx
    }

    fn get_commit_statuses(
        &self,
        reqid: String,
    ) -> oneshot::Receiver<CommittorServiceResult<Vec<CommitStatusRow>>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let reqid = reqid.parse::<u64>().unwrap();
        let status_rows =
            if self.commit_statuses.lock().unwrap().contains_key(&reqid) {
                self.commit_statuses
                    .lock()
                    .unwrap()
                    .get(&reqid)
                    .unwrap()
                    .clone()
            } else {
                // TODO: @@@ why are we faking this here?
                let commit =
                    self.committed_changesets.lock().unwrap().remove(&reqid);
                let Some((changeset, hash, finalize)) = commit else {
                    tx.send(Ok(vec![])).unwrap_or_else(|_| {
                        error!("Failed to send commit status response");
                    });
                    return rx;
                };
                changeset
                    .accounts
                    .iter()
                    .map(|(pubkey, acc)| CommitStatusRow {
                        reqid: reqid.to_string(),
                        pubkey: *pubkey,
                        delegated_account_owner: acc.owner(),
                        slot: changeset.slot,
                        ephemeral_blockhash: hash,
                        undelegate: changeset
                            .accounts_to_undelegate
                            .contains(pubkey),
                        lamports: acc.lamports(),
                        finalize,
                        data: Some(acc.data().to_vec()),
                        commit_type: CommitType::DataAccount,
                        created_at: now(),
                        commit_status: CommitStatus::Succeeded((
                            reqid,
                            CommitStrategy::FromBuffer,
                            CommitStatusSignatures {
                                process_signature: Signature::new_unique(),
                                finalize_signature: Some(
                                    Signature::new_unique(),
                                ),
                                undelegate_signature: None,
                            },
                        )),
                        last_retried_at: now(),
                        retries_count: 0,
                    })
                    .collect()
            };
        tx.send(Ok(status_rows)).unwrap_or_else(|_| {
            error!("Failed to send commit status response");
        });
        rx
    }

    fn get_bundle_signatures(
        &self,
        bundle_id: u64,
    ) -> tokio::sync::oneshot::Receiver<
        crate::error::CommittorServiceResult<Option<BundleSignatureRow>>,
    > {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let bundle_signature = BundleSignatureRow {
            bundle_id,
            processed_signature: Signature::new_unique(),
            finalized_signature: Some(Signature::new_unique()),
            undelegate_signature: None,
            created_at: now(),
        };
        tx.send(Ok(Some(bundle_signature))).unwrap_or_else(|_| {
            error!("Failed to send bundle signatures response");
        });
        rx
    }

    fn reserve_pubkeys_for_committee(
        &self,
        committee: Pubkey,
        owner: Pubkey,
    ) -> oneshot::Receiver<CommittorServiceResult<()>> {
        let (tx, rx) =
            tokio::sync::oneshot::channel::<CommittorServiceResult<()>>();
        self.reserved_pubkeys_for_committee
            .lock()
            .unwrap()
            .insert(committee, owner);
        tx.send(Ok(())).unwrap_or_else(|_| {
            error!("Failed to send response");
        });
        rx
    }

    fn get_reqids(
        &self,
    ) -> oneshot::Receiver<CommittorServiceResult<HashSet<String>>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let reqids = self
            .commit_statuses
            .lock()
            .unwrap()
            .keys()
            .map(|&id| id.to_string())
            .collect();
        tx.send(Ok(reqids)).unwrap_or_else(|_| {
            error!("Failed to send get_reqids response");
        });
        rx
    }

    fn remove_commit_statuses_with_reqid(
        &self,
        reqid: String,
    ) -> oneshot::Receiver<CommittorServiceResult<usize>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let reqid = reqid.parse::<u64>().unwrap();
        let removed_count = self
            .commit_statuses
            .lock()
            .unwrap()
            .remove(&reqid)
            .map_or(0, |statuses| statuses.len());
        tx.send(Ok(removed_count)).unwrap_or_else(|_| {
            error!("Failed to send remove_commit_statuses response");
        });
        rx
    }

    fn run_validator_signed_ixs(
        &self,
        ixs: Vec<Instruction>,
    ) -> oneshot::Receiver<CommittorServiceResult<()>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.validator_signed_ixs.lock().unwrap().extend(ixs);
        tx.send(Ok(())).unwrap_or_else(|_| {
            error!("Failed to send run_validator_signed_ixs response");
        });
        rx
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards")
        .as_secs()
}
