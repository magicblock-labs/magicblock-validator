#![cfg(test)]

use log::*;
use magicblock_committor_program::ChangedAccount;
use std::{collections::HashMap, sync::Arc};
use test_tools_core::init_logger;

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

fn add_success(cc: &ChangesetCommittorStub, reqid: u64) -> CommitStatusRow {
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
    )
}

fn add_pending(cc: &ChangesetCommittorStub, reqid: u64) -> CommitStatusRow {
    add_with_status(cc, reqid, CommitStatus::Pending)
}

fn add_with_status(
    cc: &ChangesetCommittorStub,
    reqid: u64,
    status: CommitStatus,
) -> CommitStatusRow {
    let row = CommitStatusRow {
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
    };
    cc.add_commit_status(row.clone());
    row
}

mod successes {
    use super::*;

    #[tokio::test]
    async fn test_retry_all_commits_succeeded() {
        init_logger!();

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
                completed: vec![
                    (reqid1.to_string(), 2),
                    (reqid2.to_string(), 1)
                ]
                .into_iter()
                .collect(),
                retried: HashMap::new(),
            }
        );

        // The correct rows were removed from the committor db
        let reqids = cc.get_reqids().await.unwrap().unwrap();
        assert!(reqids.is_empty());
    }

    #[tokio::test]
    async fn test_retry_two_commits_succeeded_one_pending() {
        init_logger!();

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
                retried: HashMap::new(),
            }
        );

        // The correct rows were removed from the committor db
        let reqids = cc.get_reqids().await.unwrap().unwrap();
        assert_eq!(reqids, vec![reqid2.to_string()].into_iter().collect());
    }
}

mod failures {
    use crate::retry::retry_service::RetryKind;

    use super::*;

    #[tokio::test]
    async fn test_retry_single_totally_failed_commit() {
        single_retry_requiring_process(CommitStatus::Failed(1)).await;
    }

    #[tokio::test]
    async fn test_retry_single_commit_failed_with_buffer_partially_initialized()
    {
        single_retry_requiring_process_and_buffer_close(
            CommitStatus::BufferAndChunkPartiallyInitialized(1),
        )
        .await;
    }

    #[tokio::test]
    async fn test_retry_single_commit_failed_process_from_buffer() {
        single_retry_requiring_process_and_buffer_close(
            CommitStatus::FailedProcess((1, CommitStrategy::FromBuffer, None)),
        )
        .await;
    }

    #[tokio::test]
    async fn test_retry_single_commit_failed_process_args() {
        single_retry_requiring_process(CommitStatus::FailedProcess((
            1,
            CommitStrategy::Args,
            None,
        )))
        .await;
    }
    #[tokio::test]
    async fn test_retry_single_commit_failed_finalize() {}

    async fn single_retry_requiring_process(status: CommitStatus) {
        init_logger!();

        let cc = Arc::new(ChangesetCommittorStub::default());

        let reqid1 = 1;
        let row1 = add_with_status(&cc, reqid1, CommitStatus::Failed(reqid1));

        let sut = CommittorRetryService::new(
            cc.clone(),
            CommittorRetryServiceConfig::default(),
        );

        let result = sut.retry_failed().await.unwrap();

        // Retries the correct amount of rows for each reqid
        assert_eq!(
            result,
            RetryPendingResult {
                completed: HashMap::new(),
                retried: vec![(reqid1.to_string(), RetryKind::Process(1))]
                    .into_iter()
                    .collect(),
            }
        );
        // Does not close existing buffers
        assert!(cc.validator_signed_ixs().is_empty());

        // Recommitted the correct changeset
        assert_eq!(cc.recommitted_changesets().len(), 1,);

        let changesets = cc.recommitted_changesets();
        let (changeset, ephemeral_blockhash, finalize) =
            changesets.get(&reqid1.to_string()).unwrap();

        assert_eq!(ephemeral_blockhash, &row1.ephemeral_blockhash);
        assert_eq!(finalize, &row1.finalize);
        assert_eq!(changeset.accounts.len(), 1);
        assert_eq!(
            changeset.accounts.get(&row1.pubkey).unwrap(),
            &ChangedAccount::Full {
                lamports: row1.lamports,
                owner: row1.delegated_account_owner,
                data: row1.data.unwrap(),
                bundle_id: 1,
            }
        );
    }

    async fn single_retry_requiring_process_and_buffer_close(
        status: CommitStatus,
    ) {
        init_logger!();

        let cc = Arc::new(ChangesetCommittorStub::default());

        let reqid1 = 1;
        let row1 = add_with_status(
            &cc,
            reqid1,
            CommitStatus::BufferAndChunkPartiallyInitialized(reqid1),
        );

        let sut = CommittorRetryService::new(
            cc.clone(),
            CommittorRetryServiceConfig::default(),
        );

        let result = sut.retry_failed().await.unwrap();

        // Retries the correct amount of rows for each reqid
        assert_eq!(
            result,
            RetryPendingResult {
                completed: HashMap::new(),
                retried: vec![(reqid1.to_string(), RetryKind::Process(1))]
                    .into_iter()
                    .collect(),
            }
        );
        // Closes existing buffers
        assert_eq!(cc.validator_signed_ixs().len(), 1);

        // Recommitted the correct changeset
        assert_eq!(cc.recommitted_changesets().len(), 1,);

        let changesets = cc.recommitted_changesets();
        let (changeset, ephemeral_blockhash, finalize) =
            changesets.get(&reqid1.to_string()).unwrap();

        assert_eq!(ephemeral_blockhash, &row1.ephemeral_blockhash);
        assert_eq!(finalize, &row1.finalize);
        assert_eq!(changeset.accounts.len(), 1);
        assert_eq!(
            changeset.accounts.get(&row1.pubkey).unwrap(),
            &ChangedAccount::Full {
                lamports: row1.lamports,
                owner: row1.delegated_account_owner,
                data: row1.data.unwrap(),
                bundle_id: 1,
            }
        );
    }

    async fn single_retry_requiring_finalize(status: CommitStatus) {
        init_logger!();

        let cc = Arc::new(ChangesetCommittorStub::default());

        let reqid1 = 1;
        let row1 = add_with_status(&cc, reqid1, CommitStatus::Failed(reqid1));

        let sut = CommittorRetryService::new(
            cc.clone(),
            CommittorRetryServiceConfig::default(),
        );

        let result = sut.retry_failed().await.unwrap();

        // Retries the correct amount of rows for each reqid
        assert_eq!(
            result,
            RetryPendingResult {
                completed: HashMap::new(),
                retried: vec![(reqid1.to_string(), RetryKind::Process(1))]
                    .into_iter()
                    .collect(),
            }
        );
        // Does not close existing buffers
        assert!(cc.validator_signed_ixs().is_empty());

        // Did not recommit the changeset
        assert!(cc.recommitted_changesets().is_empty());
    }
}
