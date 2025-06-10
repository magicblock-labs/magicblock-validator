#![cfg(test)]

use std::sync::Arc;

use solana_pubkey::Pubkey;
use solana_sdk::hash::Hash;

use crate::{
    persist::{
        CommitStatus, CommitStatusRow, CommitStatusSignatures, CommitStrategy,
        CommitType,
    },
    retry::retry_service::{
        CommittorRetryService, CommittorRetryServiceConfig, RetryPendingResult,
    },
    stubs::ChangesetCommittorStub,
    ChangesetCommittor,
};

fn add_success(cc: &ChangesetCommittorStub, reqid: u64) {
    add_with_status(
        cc,
        reqid,
        CommitStatus::Succeeded((
            0,
            CommitStrategy::Args,
            CommitStatusSignatures {
                process_signature: Default::default(),
                finalize_signature: None,
                undelegate_signature: None,
            },
        )),
    );
}

fn add_pending(cc: &ChangesetCommittorStub, reqid: u64) {
    add_with_status(cc, reqid, CommitStatus::Pending);
}

fn add_with_status(
    cc: &ChangesetCommittorStub,
    reqid: u64,
    status: CommitStatus,
) {
    cc.add_commit_status(CommitStatusRow {
        reqid: reqid.to_string(),
        pubkey: Pubkey::new_unique(),
        commit_type: CommitType::DataAccount,
        delegated_account_owner: Pubkey::new_unique(),
        slot: 0,
        ephemeral_blockhash: Hash::new_unique(),
        undelegate: false,
        lamports: 100,
        finalize: true,
        data: Some(vec![1, 2, 3, 4, 5]),
        created_at: 0,
        commit_status: status,
        last_retried_at: 0,
        retries_count: 0,
    });
}

#[tokio::test]
async fn test_retry_all_commits_succeeded() {
    let cc = Arc::new(ChangesetCommittorStub::default());

    let reqid1 = 1;
    let reqid2 = 2;
    add_success(&cc, reqid1);
    add_success(&cc, reqid1);
    add_success(&cc, reqid2);

    let sut = CommittorRetryService::new(
        cc.clone(),
        CommittorRetryServiceConfig::default(),
    );
    let result = sut.retry_failed().await.unwrap();

    // Removes the correct amount of rows for each reqid
    assert_eq!(
        result,
        RetryPendingResult {
            completed: vec![(reqid1.to_string(), 2), (reqid2.to_string(), 1)]
                .into_iter()
                .collect(),
            failed: vec![],
        }
    );

    // The correct rows were removed from the committor db
    let reqids = cc.get_reqids().await.unwrap().unwrap();
    assert!(reqids.is_empty());
}

#[tokio::test]
async fn test_retry_two_commits_succeeded_one_pending() {
    let cc = Arc::new(ChangesetCommittorStub::default());

    let reqid1 = 1;
    let reqid2 = 2;
    add_success(&cc, reqid1);
    add_success(&cc, reqid1);
    add_pending(&cc, reqid2);

    let sut = CommittorRetryService::new(
        cc.clone(),
        CommittorRetryServiceConfig::default(),
    );
    let result = sut.retry_failed().await.unwrap();

    // Removes the correct amount of rows for each reqid
    assert_eq!(
        result,
        RetryPendingResult {
            completed: vec![(reqid1.to_string(), 2)].into_iter().collect(),
            failed: vec![],
        }
    );

    // The correct rows were removed from the committor db
    let reqids = cc.get_reqids().await.unwrap().unwrap();
    assert_eq!(reqids, vec![reqid2.to_string()].into_iter().collect());
}

#[tokio::test]
async fn test_retry_single_totally_failed_commit() {
    let cc = Arc::new(ChangesetCommittorStub::default());

    let reqid1 = 1;
    add_with_status(&cc, reqid1, CommitStatus::Failed(reqid1));

    let sut = CommittorRetryService::new(
        cc.clone(),
        CommittorRetryServiceConfig::default(),
    );

    let result = sut.retry_failed().await.unwrap();
    eprintln!("[test_retry_single_totally_failed_commit] result: {result:?}");
}
