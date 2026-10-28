use std::{collections::HashSet, sync::Arc, time::Duration};

use delegation_program_interface as dpi;
use mbv_core::intent::types::CommittedAccount;
use program::outbox::{ExecutionStage, TwoStageProgress};
use rpc_client::MagicblockRpcClient;
use solana_account::Account;
use solana_rpc_client::{mock_sender::MocksMap, nonblocking::rpc_client::RpcClient};
use solana_rpc_client_api::request::RpcRequest;

use super::*;
use crate::{
    execution::{cleanup::clean, test_utils::*},
    shared::tasks::{
        UndelegateTask,
        commit_delivery::CommitDelivery,
        utils::{TransactionUtils, create_action_tasks, create_commit_finalize_task},
    },
};

fn commit() -> Task {
    let mut task = create_commit_finalize_task(
        2,
        true,
        CommittedAccount {
            pubkey: ACCOUNT,
            account: Account {
                data: vec![1; 512],
                ..Account::default()
            },
            remote_slot: 0,
        },
        None,
    );
    task.delivery = CommitDelivery::StateInBuffer;
    task.into()
}

fn undelegate() -> Task {
    Task::Undelegate(UndelegateTask {
        version: Default::default(),
        delegated_account: ACCOUNT,
        owner_program: Pubkey::new_unique(),
        rent_reimbursement: Pubkey::new_unique(),
        include_undelegation_request: false,
    })
}

fn action() -> Task {
    create_action_tasks(&[base_action()]).next().unwrap()
}

fn strategy(tasks: Vec<Task>) -> TransactionStrategy {
    TransactionStrategy {
        tasks,
        lookup_tables_keys: vec![RESERVED_ALT_KEY],
        ..Default::default()
    }
}

fn split(follow_up: Vec<Task>) -> TransactionPlan {
    TransactionPlan {
        current: strategy(vec![commit()]),
        follow_up: Some(strategy(follow_up)),
    }
}

#[tokio::test(start_paused = true)]
async fn timeout_preserves_actions_and_resolves_pending_transactions() {
    for phase in ["combined", "commit", "follow_up"] {
        for has_pending_transaction in [false, true] {
            let steps = if has_pending_transaction { vec![] } else { vec![Preparation::Wait] };
            let mut ctx = executor(steps, None);
            let pending = PendingTransaction {
                signature: Signature::new_unique(),
                blockhash: solana_hash::Hash::default(),
            };
            if has_pending_transaction {
                // The signed transaction may still land. Its first status is unknown;
                // it confirms after the callback deadline while resolving the same signature.
                let mut mocks = MocksMap::default();
                for _ in 0..2 {
                    mock_response(&mut mocks, RpcRequest::IsBlockhashValid, true);
                }
                mock_response(&mut mocks, RpcRequest::GetSignatureStatuses, "[null]");
                let rpc = MagicblockRpcClient::from(RpcClient::new_mock_with_mocks_map(
                    "succeeds", mocks,
                ));
                ctx.intent_client = crate::execution::client::IntentExecutionClient::new(rpc, 0);
            }
            let mut report = IntentExecutionReport::default();
            let earlier_commit = Signature::new_unique();
            let mut runner = match phase {
                "combined" => TransactionExecutor::new(
                    &ctx,
                    &mut report,
                    42,
                    &[ACCOUNT],
                    strategy(vec![commit(), action()]).into(),
                ),
                "commit" => TransactionExecutor::new(
                    &ctx,
                    &mut report,
                    42,
                    &[ACCOUNT],
                    split(vec![undelegate(), action()]),
                ),
                _ => TransactionExecutor::resume_follow_up(
                    &ctx,
                    &mut report,
                    42,
                    earlier_commit,
                    strategy(vec![undelegate(), action()]),
                ),
            };
            if has_pending_transaction {
                runner.pending_transaction = Some(pending);
            }
            let deadline = tokio::time::Instant::now() + Duration::from_millis(10);
            let result = runner
                .execute(|| deadline.checked_duration_since(tokio::time::Instant::now()))
                .await
                .unwrap();
            let callbacks = ctx.actions_callback_executor.0.lock().unwrap();
            assert!(
                matches!(
                    callbacks.as_slice(),
                    [(None, Err(ActionError::TimeoutError), _)]
                ),
                "{phase}, pending={has_pending_transaction}"
            );
            let prepared = ctx.transaction_preparator.prepared.lock().unwrap();
            assert_eq!(
                prepared.len(),
                usize::from(phase == "commit") + if has_pending_transaction { 0 } else { 2 }
            );
            assert!(prepared.iter().all(|tasks| !tasks.is_empty()));
            assert!(
                prepared.is_empty()
                    || prepared
                        .iter()
                        .any(|tasks| tasks.iter().any(|task| matches!(task, Task::BaseAction(_)))),
                "timeout must retain actions"
            );
            match result.confirmed_commit {
                None => {
                    assert_eq!(phase, "combined");
                    if has_pending_transaction {
                        assert_eq!(result.signature, pending.signature);
                    }
                }
                Some(commit_signature) => {
                    assert_ne!(phase, "combined");
                    if phase == "follow_up" {
                        assert_eq!(commit_signature, earlier_commit);
                        if has_pending_transaction {
                            assert_eq!(result.signature, pending.signature);
                        }
                    } else if has_pending_transaction {
                        assert_eq!(commit_signature, pending.signature);
                    }
                    if let Some(ExecutionStage::TwoStage(TwoStageProgress::Finalizing {
                        commit,
                        ..
                    })) = ctx.outbox_client.stages.lock().unwrap().last()
                    {
                        assert_eq!(*commit, commit_signature);
                    }
                }
            }
        }
    }
}

#[tokio::test(start_paused = true)]
async fn timeout_keeps_attempts_already_spent_on_current_transaction() {
    let executor = executor(vec![Preparation::Wait], None);
    let mut report = IntentExecutionReport::default();
    let mut runner = TransactionExecutor::new(
        &executor,
        &mut report,
        42,
        &[ACCOUNT],
        strategy(vec![commit(), action()]).into(),
    );
    runner.current_attempt = 8;
    runner
        .execute_with_timeout(Some(Duration::from_millis(10)))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(runner.current_attempt, 10);
}

#[tokio::test]
async fn recovery_preserves_progress_and_resources() {
    let nonce_error = format!(
        r#"{{"InstructionError":[{},{{"Custom":{}}}]}}"#,
        TransactionUtils::COMPUTE_BUDGET_INSTRUCTION_COUNT,
        dpi::error::DlpError::NonceOutOfOrder as u32
    );
    // The RPC mock has no delegation record, so nonce recovery fails after
    // preparing the first transaction and receiving its on-chain nonce error.
    for (steps, error) in [
        (vec![Preparation::Fail], None),
        (vec![Preparation::Ready], Some(nonce_error.as_str())),
        (vec![Preparation::Ready, Preparation::Fail], None),
    ] {
        let expected_preparations = steps.len();
        let executor = executor(steps, error);
        let mut report = IntentExecutionReport::default();
        let result = TransactionExecutor::new(
            &executor,
            &mut report,
            42,
            &[ACCOUNT],
            split(vec![undelegate(), action()]),
        )
        .execute(|| Some(Duration::from_secs(60)))
        .await;
        if error.is_some() {
            assert!(
                matches!(result, Err(IntentExecutorError::TaskBuilderError(_))),
                "{result:?}"
            );
        } else {
            assert!(
                matches!(result, Err(IntentExecutorError::FailedPreparation(_))),
                "{result:?}"
            );
        }
        {
            let prepared = executor.transaction_preparator.prepared.lock().unwrap();
            assert_eq!(prepared.len(), expected_preparations);
            assert!(matches!(prepared[0].as_slice(), [Task::CommitFinalize(_)]));
            if expected_preparations == 2 {
                assert!(matches!(
                    prepared[1].as_slice(),
                    [Task::Undelegate(_), Task::BaseAction(_)]
                ));
            }
            assert!(executor.actions_callback_executor.0.lock().unwrap().is_empty());
            assert_eq!(report.cleanup.len(), expected_preparations);
            assert_eq!(report.cleanup[0].buffers[0].pubkey, ACCOUNT);
            assert_eq!(report.cleanup[0].buffers[0].commit_id, 2);
            assert!(
                report
                    .cleanup
                    .iter()
                    .all(|resources| resources.alt_reservation.contains(&RESERVED_ALT_KEY))
            );
        }
        clean(
            &executor.authority,
            &executor.transaction_preparator,
            report.cleanup,
            false,
        )
        .await
        .unwrap();
        assert!(
            executor
                .transaction_preparator
                .cleaned
                .lock()
                .unwrap()
                .iter()
                .all(|(_, close_buffers)| !close_buffers)
        );
    }

    // Rejected actions and undelegations can leave commits or follow-up work.
    // Action-only intents must fail without sending a replacement transaction.
    let failed = &[(0, false)][..];
    for (tasks, confirmed, failed_task, succeeds, remaining, expected_callbacks) in [
        (vec![commit(), action()], false, 1, true, Some(1), failed),
        (vec![action()], false, 0, false, None, failed),
        (vec![undelegate(), action()], true, 1, true, Some(1), failed),
        (vec![action()], true, 0, true, None, failed),
        (
            vec![commit(), undelegate(), action()],
            false,
            1,
            true,
            Some(1),
            failed,
        ),
        (vec![undelegate(), action()], true, 0, false, None, failed),
        (
            vec![commit(), action(), undelegate(), action()],
            false,
            2,
            true,
            Some(2),
            &[(1, false), (0, true)],
        ),
    ] {
        let callback_destinations: Vec<_> = tasks
            .iter()
            .filter_map(|task| match task {
                Task::BaseAction(task) => {
                    Some(task.action().callback.as_ref().unwrap().destination_program)
                }
                _ => None,
            })
            .collect();
        let undelegation_failed = matches!(tasks[failed_task], Task::Undelegate(_));
        let instruction = TransactionUtils::COMPUTE_BUDGET_INSTRUCTION_COUNT as usize + failed_task;
        let error = format!(r#"{{"InstructionError":[{instruction},{{"Custom":1}}]}}"#);
        let mut ctx = executor(vec![], Some(&error));
        let has_commits = tasks.iter().any(|task| matches!(task, Task::CommitFinalize(_)));
        if !has_commits {
            let outbox = Arc::new(RecordingOutbox::default());
            ctx.context.outbox_client = outbox.clone();
            ctx.outbox_client = outbox;
        }
        let mut report = IntentExecutionReport::default();
        let earlier_commit = Signature::new_unique();
        let mut strategy = strategy(tasks);
        strategy.uniqueness_nonce = Some(42);
        let runner = if confirmed {
            TransactionExecutor::resume_follow_up(&ctx, &mut report, 42, earlier_commit, strategy)
        } else {
            TransactionExecutor::new(
                &ctx,
                &mut report,
                42,
                if has_commits { &[ACCOUNT] } else { &[] },
                strategy.into(),
            )
        };
        let result = runner.execute(|| Some(Duration::from_secs(60))).await;
        assert_eq!(
            result.is_ok(),
            succeeds,
            "confirmed={confirmed}, task={failed_task}: {result:?}"
        );
        assert_eq!(
            report.progress.confirmed_commit,
            confirmed.then_some(earlier_commit)
        );
        {
            let callbacks = ctx.actions_callback_executor.0.lock().unwrap();
            assert_eq!(callbacks.len(), expected_callbacks.len());
            for ((signature, result, destination), &(index, succeeds)) in
                callbacks.iter().zip(expected_callbacks)
            {
                assert_eq!(*destination, callback_destinations[index]);
                assert_eq!(result.is_ok(), succeeds);
                if undelegation_failed && !succeeds {
                    assert!(matches!(result, Err(ActionError::IntentFailedError(_))));
                    assert_eq!(
                        *signature,
                        Some(
                            ctx.outbox_client.stages.lock().unwrap()[0]
                                .pending_transaction()
                                .signature
                        )
                    );
                }
            }
        }
        {
            let prepared = ctx.transaction_preparator.prepared.lock().unwrap();
            assert_eq!(prepared.len(), 1 + usize::from(remaining.is_some()));
            if let Some(remaining) = remaining {
                assert_eq!(prepared[1].len(), remaining);
                if confirmed {
                    assert!(matches!(prepared[1][0], Task::Undelegate(_)));
                } else {
                    assert!(matches!(prepared[1][0], Task::CommitFinalize(_)));
                }
            }
            assert_eq!(report.cleanup.len(), prepared.len());
        }
        clean(
            &ctx.authority,
            &ctx.transaction_preparator,
            report.cleanup,
            succeeds,
        )
        .await
        .unwrap();
        let cleaned = ctx.transaction_preparator.cleaned.lock().unwrap();
        assert!(cleaned.iter().all(|(resources, close_buffers)| {
            *close_buffers == succeeds
                && resources.uniqueness_nonce == Some(42)
                && resources.alt_reservation.contains(&RESERVED_ALT_KEY)
        }));
        assert_eq!(
            cleaned.iter().map(|(resources, _)| resources.buffers.len()).sum::<usize>(),
            usize::from(has_commits)
        );
    }

    // Exercise real reservation counts: failed and cancelled preparation must
    // release only what it acquired, without consuming another intent's hold.
    use crate::execution::preparation::{Preparator, cleanup_resources::CleanupResources};
    let authority = solana_keypair::Keypair::new();
    let rpc = MagicblockRpcClient::from(RpcClient::new_mock("succeeds".to_owned()));
    let tables = table_mania::TableMania::new(rpc.clone(), &authority, None);
    let shared = HashSet::from([RESERVED_ALT_KEY]);
    let mut other_intent = HashSet::new();
    tables.reserve_pubkeys(&authority, &shared, &mut other_intent).await.unwrap();

    let mut resources = Vec::new();
    for _ in 0..2 {
        let cleanup = resources.push_mut(CleanupResources::default());
        tables
            .reserve_pubkeys(&authority, &shared, &mut cleanup.alt_reservation)
            .await
            .unwrap();
    }
    let requested = HashSet::from([RESERVED_ALT_KEY, Pubkey::new_unique()]);
    let failed = resources.push_mut(CleanupResources::default());
    assert!(
        tables
            .reserve_pubkeys(
                &solana_keypair::Keypair::new(),
                &requested,
                &mut failed.alt_reservation
            )
            .await
            .is_err()
    );
    assert_eq!(failed.alt_reservation, shared);

    let cancelled = resources.push_mut(CleanupResources::default());
    let locked_tables = tables.active_tables.read().await;
    // Existing keys can be reserved under the read lock. Adding the missing key
    // waits for the write lock, giving cancellation a deterministic boundary.
    assert!(
        timeout(
            Duration::from_millis(1),
            tables.reserve_pubkeys(&authority, &requested, &mut cancelled.alt_reservation),
        )
        .await
        .is_err()
    );
    drop(locked_tables);
    assert_eq!(cancelled.alt_reservation, shared);
    assert_eq!(tables.get_pubkey_refcount(&RESERVED_ALT_KEY).await, Some(5));

    let preparator = Preparator::new(rpc, tables.clone(), crate::ComputeBudgetConfig::new(0));
    clean(&authority, &preparator, resources, false).await.unwrap();
    assert_eq!(tables.get_pubkey_refcount(&RESERVED_ALT_KEY).await, Some(1));
    tables.release_pubkeys(&other_intent).await;
    assert_eq!(tables.active_tables_count().await, 0);
}
