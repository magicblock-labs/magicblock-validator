use std::{collections::HashMap, sync::Arc};

use delegation_program_api::state::{DelegationMetadata, UndelegationRequester};
use program::outbox::{ExecutionStage, PendingTransaction, TwoStageProgress};
use rpc_client::MagicblockRpcClient;
use solana_account::Account;
use solana_pubkey::Pubkey;
use solana_rpc_client::{mock_sender::MocksMap, nonblocking::rpc_client::RpcClient};
use solana_rpc_client_api::request::RpcRequest;
use solana_signature::Signature;

use super::*;
use crate::{
    intent_engine::intent_scheduler::create_test_intent_bundle,
    intent_executor::{intent_execution_client::IntentExecutionClient, test_utils::*},
    tasks::{
        BaseTaskImpl,
        task_info_fetcher::{CacheTaskInfoFetcher, TaskInfoFetcherResult},
    },
};

struct InfoFetcher;

#[async_trait]
impl TaskInfoFetcher for InfoFetcher {
    async fn fetch_next_commit_nonces(
        &self,
        _: &[AccountSnapshot],
        _: u64,
    ) -> TaskInfoFetcherResult<HashMap<Pubkey, u64>> {
        unreachable!("the cache allocates the next nonce")
    }

    async fn fetch_current_commit_nonces(
        &self,
        accounts: &[AccountSnapshot],
        _: u64,
    ) -> TaskInfoFetcherResult<HashMap<Pubkey, u64>> {
        Ok(accounts.iter().map(|(key, _)| (*key, 1)).collect())
    }

    async fn fetch_delegation_metadata(
        &self,
        accounts: &[AccountSnapshot],
        _: u64,
    ) -> TaskInfoFetcherResult<HashMap<Pubkey, DelegationMetadata>> {
        Ok(accounts
            .iter()
            .map(|(key, _)| {
                (
                    *key,
                    DelegationMetadata {
                        last_commit_id: 1,
                        undelegation_requester: UndelegationRequester::None,
                        seeds: vec![],
                        rent_payer: *key,
                    },
                )
            })
            .collect())
    }

    async fn get_base_accounts(
        &self,
        keys: &[Pubkey],
        _: u64,
    ) -> TaskInfoFetcherResult<HashMap<Pubkey, Account>> {
        assert!(keys.is_empty(), "small test accounts need no diff lookup");
        Ok(HashMap::new())
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Resolution {
    Confirmed,
    Rejected,
    Expired,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Failure {
    None,
    Preparation,
    Reporting,
}

#[derive(Clone, Copy, PartialEq)]
enum IntentKind {
    CommitAndUndelegate,
    Actions,
    CommitOnly,
}

#[tokio::test(start_paused = true)]
async fn intent_execution_preserves_recovery_and_completion() {
    use Failure::{None, Preparation as PreparationFailure, Reporting};
    use IntentKind::{Actions, CommitAndUndelegate, CommitOnly};
    use Resolution::{Confirmed, Expired, Rejected, Unavailable};

    let pending = PendingTransaction {
        signature: Signature::new_unique(),
        blockhash: Default::default(),
    };
    let earlier_commit = Signature::new_unique();
    let accepted = OutboxIntentBundleStatus::Accepted;
    let combined = OutboxIntentBundleStatus::Executing(ExecutionStage::SingleStage(pending));
    let committing = OutboxIntentBundleStatus::Executing(ExecutionStage::TwoStage(
        TwoStageProgress::Committing(pending),
    ));
    let following = OutboxIntentBundleStatus::Executing(ExecutionStage::TwoStage(
        TwoStageProgress::Finalizing {
            commit: earlier_commit,
            finalize: pending,
        },
    ));
    // Expected task counts per prepared transaction: (commits, undelegations, actions).
    let combined_tasks: &[(usize, usize, usize)] = &[(2, 1, 0)];
    let split_tasks: &[(usize, usize, usize)] = &[(2, 0, 0), (0, 1, 0)];
    let follow_up_tasks: &[(usize, usize, usize)] = &[(0, 1, 0)];
    for (status, resolution, failure, expected_tasks, kind) in [
        (
            &accepted,
            Confirmed,
            None,
            combined_tasks,
            CommitAndUndelegate,
        ),
        (&accepted, Confirmed, None, &[(0, 0, 1)], Actions),
        (&combined, Expired, None, &[(0, 0, 1)], Actions),
        (&combined, Confirmed, None, &[], CommitAndUndelegate),
        (
            &combined,
            Rejected,
            None,
            combined_tasks,
            CommitAndUndelegate,
        ),
        (
            &committing,
            Confirmed,
            None,
            follow_up_tasks,
            CommitAndUndelegate,
        ),
        (
            &committing,
            Rejected,
            None,
            split_tasks,
            CommitAndUndelegate,
        ),
        (&following, Confirmed, None, &[], CommitAndUndelegate),
        (
            &following,
            Rejected,
            None,
            follow_up_tasks,
            CommitAndUndelegate,
        ),
        (&combined, Unavailable, None, &[], CommitAndUndelegate),
        (
            &accepted,
            Confirmed,
            PreparationFailure,
            combined_tasks,
            CommitAndUndelegate,
        ),
        (
            &following,
            Rejected,
            PreparationFailure,
            follow_up_tasks,
            CommitAndUndelegate,
        ),
        (
            &accepted,
            Confirmed,
            Reporting,
            combined_tasks,
            CommitAndUndelegate,
        ),
        (&committing, Confirmed, None, &[], CommitOnly),
    ] {
        let actions_only = kind == Actions;
        let undelegated = Pubkey::new_unique();
        let undelegations = if kind == CommitAndUndelegate { vec![undelegated] } else { vec![] };
        let mut intent = create_test_intent_bundle(42, &[ACCOUNT], &undelegations).inner;
        if actions_only {
            intent.intent_bundle = Default::default();
            intent.intent_bundle.standalone_actions.push(base_action());
        }
        let info = Arc::new(CacheTaskInfoFetcher::new(InfoFetcher));
        // Seed both nonces so completion must retain one and invalidate the other.
        info.fetch_current_commit_nonces(&[(ACCOUNT, 0), (undelegated, 0)], 0)
            .await
            .unwrap();
        let mut mocks = MocksMap::default();
        if matches!(status, OutboxIntentBundleStatus::Executing(_)) {
            let statuses = match resolution {
                Confirmed => vec![],
                Rejected => vec![
                    r#"[{"slot":1,"confirmations":null,"err":"AccountNotFound","status":{"Err":"AccountNotFound"},"confirmationStatus":"finalized"}]"#,
                ],
                Expired => vec!["[null]"],
                Unavailable => vec!["[]"; 5],
            };
            for value in statuses {
                mock_response(
                    &mut mocks,
                    RpcRequest::IsBlockhashValid,
                    resolution != Expired,
                );
                mock_response(&mut mocks, RpcRequest::GetSignatureStatuses, value);
            }
            if resolution == Confirmed {
                mock_response(&mut mocks, RpcRequest::IsBlockhashValid, true);
            }
        }
        let rpc = MagicblockRpcClient::from(RpcClient::new_mock_with_mocks_map("succeeds", mocks));
        let base = executor(
            if failure == PreparationFailure { vec![Preparation::Fail] } else { vec![] },
            Option::None,
        );
        let outbox = Arc::new(RecordingOutbox {
            expected_commit_nonces: if actions_only {
                vec![]
            } else {
                std::iter::once(ACCOUNT)
                    .chain(undelegations.iter().copied())
                    .map(|key| (key, 2))
                    .collect()
            },
            fail_reporting: failure == Reporting,
            ..Default::default()
        });
        if let OutboxIntentBundleStatus::Executing(stage) = status {
            outbox.stages.lock().unwrap().push(stage.clone());
        }
        let preparator = base.transaction_preparator.clone();
        let callbacks = base.actions_callback_executor.clone();
        let ctx = IntentExecutorCtx {
            authority: base.authority,
            intent_client: IntentExecutionClient::new(rpc, 0),
            transaction_preparator: base.transaction_preparator,
            task_info_fetcher: info.clone(),
            outbox_client: outbox.clone(),
            actions_callback_executor: base.actions_callback_executor,
        };
        let (result, cleanup) = Box::new(IntentExecutorImpl::new(
            ctx,
            status.clone(),
            Duration::from_secs(60),
        ))
        .execute(intent)
        .await;
        let succeeds = failure == None && resolution != Unavailable;
        assert_eq!(
            result.is_ok(),
            succeeds,
            "{status:?}, {resolution:?}, {failure:?}: {:?}",
            result.inner
        );
        match failure {
            PreparationFailure => assert!(matches!(
                result.inner,
                Err(IntentExecutorError::FailedPreparation(_))
            )),
            Reporting => assert!(matches!(
                result.inner,
                Err(IntentExecutorError::OutboxClientError(_))
            )),
            None if resolution == Unavailable => assert!(matches!(result.inner,
                Err(IntentExecutorError::PendingSignatureResolutionError(sig)) if sig == pending.signature)),
            None => match result.inner.as_ref().unwrap().confirmed_commit {
                Option::None => {
                    assert!(status == &accepted || status == &combined);
                    if status == &combined && expected_tasks.is_empty() {
                        assert_eq!(result.inner.as_ref().unwrap().signature, pending.signature);
                    }
                }
                Some(commit_signature) => {
                    assert!(status == &committing || status == &following);
                    if status == &following {
                        assert_eq!(commit_signature, earlier_commit);
                    } else if resolution == Confirmed {
                        assert_eq!(commit_signature, pending.signature);
                    }
                    if expected_tasks.is_empty() {
                        assert_eq!(result.inner.as_ref().unwrap().signature, pending.signature);
                    }
                }
            },
        }
        let actual_tasks: Vec<_> = preparator
            .prepared
            .lock()
            .unwrap()
            .iter()
            .map(|tasks| {
                tasks.iter().fold(
                    (0, 0, 0),
                    |(commits, undelegations, actions), task| match task {
                        BaseTaskImpl::CommitFinalize(_) => (commits + 1, undelegations, actions),
                        BaseTaskImpl::Undelegate(_) => (commits, undelegations + 1, actions),
                        BaseTaskImpl::BaseAction(_) => (commits, undelegations, actions + 1),
                    },
                )
            })
            .collect();
        assert_eq!(
            actual_tasks, expected_tasks,
            "{status:?}, {resolution:?}, {failure:?}"
        );
        if actions_only {
            assert_eq!(
                *preparator.uniqueness_nonces.lock().unwrap(),
                vec![Some(42)]
            );
        }
        assert_eq!(callbacks.0.lock().unwrap().len(), usize::from(actions_only));
        assert_eq!(result.callbacks_report.len(), usize::from(actions_only));
        assert_eq!(
            info.peek_commit_nonce(&ACCOUNT).await.is_some(),
            succeeds || actions_only
        );
        assert_eq!(
            info.peek_commit_nonce(&undelegated).await.is_some(),
            kind != CommitAndUndelegate
        );
        let mut expected_signatures = vec![];
        if let Some(stage) = outbox.stages.lock().unwrap().last() {
            if let ExecutionStage::TwoStage(TwoStageProgress::Finalizing { commit, .. }) = stage {
                expected_signatures.push(*commit);
            }
            expected_signatures.push(stage.pending_transaction().signature);
        }
        assert_eq!(
            result.progress.signatures(result.inner.as_ref().err()).collect::<Vec<_>>(),
            expected_signatures,
            "{status:?}, {resolution:?}, {failure:?}"
        );
        {
            let reports = outbox.reports.lock().unwrap();
            assert_eq!(
                reports.len(),
                usize::from(failure != PreparationFailure && resolution != Unavailable)
            );
            if let Some(sent) = reports.first() {
                assert!(sent.error_message.is_none());
                assert_eq!(sent.chain_signatures, expected_signatures);
            }
        }
        if result.is_err() {
            assert!(!result.is_retriable(true));
        }
        cleanup.clean().await.unwrap();
        let cleaned = preparator.cleaned.lock().unwrap();
        assert!(cleaned.iter().all(|(_, close_buffers)| *close_buffers == succeeds));
        assert_eq!(
            cleaned.iter().filter(|(keys, _)| keys.contains(&RESERVED_ALT_KEY)).count(),
            expected_tasks.len()
        );
    }
}
