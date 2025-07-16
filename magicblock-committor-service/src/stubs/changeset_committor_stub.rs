use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use magicblock_committor_program::Changeset;
use solana_pubkey::Pubkey;
use solana_sdk::{hash::Hash, signature::Signature};
use tokio::sync::oneshot;

use crate::{
    error::CommittorServiceResult,
    persist::{
        BundleSignatureRow, CommitStatus, CommitStatusRow,
        CommitStatusSignatures, CommitStrategy, CommitType,
    },
    L1MessageCommittor,
};

#[derive(Default)]
pub struct ChangesetCommittorStub {
    reserved_pubkeys_for_committee: Arc<Mutex<HashMap<Pubkey, Pubkey>>>,
    #[allow(clippy::type_complexity)]
    committed_changesets: Arc<Mutex<HashMap<u64, (Changeset, Hash, bool)>>>,
}

impl L1MessageCommittor for ChangesetCommittorStub {
    fn commit_l1_messages(
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
            log::error!("Failed to send commit changeset response");
        });
        rx
    }

    fn get_commit_statuses(
        &self,
        reqid: String,
    ) -> oneshot::Receiver<CommittorServiceResult<Vec<CommitStatusRow>>> {
        let reqid = reqid.parse::<u64>().unwrap();
        let commit = self.committed_changesets.lock().unwrap().remove(&reqid);
        let (tx, rx) = tokio::sync::oneshot::channel();
        let Some((changeset, hash, finalize)) = commit else {
            tx.send(Ok(vec![])).unwrap_or_else(|_| {
                log::error!("Failed to send commit status response");
            });
            return rx;
        };
        let status_rows = changeset
            .accounts
            .iter()
            .map(|(pubkey, acc)| CommitStatusRow {
                reqid: reqid.to_string(),
                pubkey: *pubkey,
                delegated_account_owner: acc.owner(),
                slot: changeset.slot,
                ephemeral_blockhash: hash,
                undelegate: changeset.accounts_to_undelegate.contains(pubkey),
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
                        finalize_signature: Some(Signature::new_unique()),
                        undelegate_signature: None,
                    },
                )),
                last_retried_at: now(),
                retries_count: 0,
            })
            .collect();
        tx.send(Ok(status_rows)).unwrap_or_else(|_| {
            log::error!("Failed to send commit status response");
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
            log::error!("Failed to send bundle signatures response");
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
            log::error!("Failed to send response");
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
