use std::time::Duration;

use mbv_core::intent::types::CommittedAccount;
use program::outbox::{ExecutionStage, TwoStageProgress};
use rpc_client::MagicblockRpcClient;
use solana_account::Account;
use solana_rpc_client::{mock_sender::MocksMap, nonblocking::rpc_client::RpcClient};
use solana_rpc_client_api::request::RpcRequest;

use super::*;
use crate::{
    intent_executor::test_utils::*,
    tasks::{
        UndelegateTask,
        utils::{TransactionUtils, create_action_tasks, create_commit_finalize_task},
    },
};

fn commit() -> BaseTaskImpl {
    create_commit_finalize_task(
        2,
        true,
        CommittedAccount {
            pubkey: ACCOUNT,
            account: Account::default(),
            remote_slot: 0,
        },
        None,
    )
    .into()
}

fn undelegate() -> BaseTaskImpl {
    BaseTaskImpl::Undelegate(UndelegateTask {
        delegated_account: ACCOUNT,
        owner_program: Pubkey::new_unique(),
        rent_reimbursement: Pubkey::new_unique(),
        include_undelegation_request: false,
    })
}

fn action() -> BaseTaskImpl {
    create_action_tasks(&[base_action()]).next().unwrap()
}

fn strategy(tasks: Vec<BaseTaskImpl>) -> TransactionStrategy {
    TransactionStrategy {
        optimized_tasks: tasks,
        ..Default::default()
    }
}

fn split(follow_up: Vec<BaseTaskImpl>) -> TransactionPlan {
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
                ctx.intent_client =
                    crate::intent_executor::intent_execution_client::IntentExecutionClient::new(
                        rpc, 0,
                    );
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
                    [(None, Err(ActionError::TimeoutError))]
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
                report
                    .junk
                    .iter()
                    .flat_map(|s| &s.optimized_tasks)
                    .any(|t| matches!(t, BaseTaskImpl::BaseAction(_))),
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
async fn execution_failure_surrenders_current_and_pending_resources() {
    let nonce_error = format!(
        r#"{{"InstructionError":[{},{{"Custom":{}}}]}}"#,
        TransactionUtils::COMPUTE_BUDGET_INSTRUCTION_COUNT,
        delegation_program_api::error::DlpError::NonceOutOfOrder as u32
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
        let prepared = executor.transaction_preparator.prepared.lock().unwrap();
        assert_eq!(prepared.len(), expected_preparations);
        assert!(matches!(
            prepared[0].as_slice(),
            [BaseTaskImpl::CommitFinalize(_)]
        ));
        if expected_preparations == 2 {
            assert!(matches!(
                prepared[1].as_slice(),
                [BaseTaskImpl::Undelegate(_), BaseTaskImpl::BaseAction(_)]
            ));
        }
        assert!(executor.actions_callback_executor.0.lock().unwrap().is_empty());
        let disposed: Vec<_> = report
            .junk
            .iter()
            .filter(|strategy| !strategy.optimized_tasks.is_empty())
            .collect();
        assert_eq!(disposed.len(), 2);
        assert!(matches!(
            disposed[1].optimized_tasks.as_slice(),
            [BaseTaskImpl::Undelegate(_), BaseTaskImpl::BaseAction(_)]
        ));
        assert_eq!(disposed[0].lookup_tables_keys, vec![RESERVED_ALT_KEY]);
        assert_eq!(
            report
                .junk
                .iter()
                .filter(|strategy| strategy.lookup_tables_keys.contains(&RESERVED_ALT_KEY))
                .count(),
            expected_preparations,
        );
    }
}
