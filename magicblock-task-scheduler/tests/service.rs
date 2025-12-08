use std::time::Duration;

use guinea::GuineaInstruction;
use magicblock_config::config::TaskSchedulerConfig;
use magicblock_program::{
    args::ScheduleTaskArgs,
    validator::{init_validator_authority_if_needed, validator_authority_id},
};
use magicblock_task_scheduler::{
    errors::TaskSchedulerResult, SchedulerDatabase, TaskSchedulerError,
    TaskSchedulerService,
};
use solana_account::ReadableAccount;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    native_token::LAMPORTS_PER_SOL,
};
use test_kit::{ExecutionTestEnv, Signer};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

type SetupResult = TaskSchedulerResult<(
    ExecutionTestEnv,
    CancellationToken,
    JoinHandle<Result<(), TaskSchedulerError>>,
)>;

async fn setup() -> SetupResult {
    let mut env = ExecutionTestEnv::new();

    init_validator_authority_if_needed(env.payers[0].insecure_clone());
    // NOTE: validator authority is unique for all tests in this file, but the payer changes for each test
    // Airdrop some SOL to the validator authority, which is used to pay task fees
    env.fund_account(validator_authority_id(), LAMPORTS_PER_SOL);

    let token = CancellationToken::new();
    let task_scheduler_db_path = SchedulerDatabase::path(
        env.ledger
            .ledger_path()
            .parent()
            .expect("ledger_path didn't have a parent, should never happen"),
    );
    let handle = TaskSchedulerService::new(
        &task_scheduler_db_path,
        &TaskSchedulerConfig::default(),
        env.transaction_scheduler.clone(),
        env.dispatch
            .tasks_service
            .take()
            .expect("Tasks service should be initialized"),
        env.ledger.latest_block().clone(),
        token.clone(),
    )?
    .start()
    .await?;

    Ok((env, token, handle))
}

#[tokio::test]
pub async fn test_schedule_task() -> TaskSchedulerResult<()> {
    let (env, token, handle) = setup().await?;

    let account =
        env.create_account_with_config(LAMPORTS_PER_SOL, 1, guinea::ID);

    // Schedule a task
    let ix = Instruction::new_with_bincode(
        guinea::ID,
        &GuineaInstruction::ScheduleTask(ScheduleTaskArgs {
            task_id: 1,
            execution_interval_millis: 10,
            iterations: 1,
            instructions: vec![Instruction::new_with_bincode(
                guinea::ID,
                &GuineaInstruction::Increment,
                vec![AccountMeta::new(account.pubkey(), false)],
            )],
        }),
        vec![
            AccountMeta::new_readonly(magicblock_magic_program_api::ID, false),
            AccountMeta::new(env.get_payer().pubkey, true),
            AccountMeta::new(account.pubkey(), false),
        ],
    );
    let txn = env.build_transaction(&[ix]);
    let result = env.execute_transaction(txn).await;
    assert!(
        result.is_ok(),
        "failed to execute schedule task transaction: {:?}",
        result
    );

    // Wait until the task scheduler actually mutates the account (with an upper bound to avoid hangs)
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if env.get_account(account.pubkey()).data().first() == Some(&1) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("task scheduler never incremented the account within 1s");

    token.cancel();
    handle.await.expect("task service join handle failed")?;

    Ok(())
}

#[tokio::test]
pub async fn test_cancel_task() -> TaskSchedulerResult<()> {
    let (env, token, handle) = setup().await?;

    let account =
        env.create_account_with_config(LAMPORTS_PER_SOL, 1, guinea::ID);

    // Schedule a task
    let task_id = 2;
    let interval = 100;
    let ix = Instruction::new_with_bincode(
        guinea::ID,
        &GuineaInstruction::ScheduleTask(ScheduleTaskArgs {
            task_id,
            execution_interval_millis: interval,
            iterations: 100,
            instructions: vec![Instruction::new_with_bincode(
                guinea::ID,
                &GuineaInstruction::Increment,
                vec![AccountMeta::new(account.pubkey(), false)],
            )],
        }),
        vec![
            AccountMeta::new_readonly(magicblock_magic_program_api::ID, false),
            AccountMeta::new(env.get_payer().pubkey, true),
            AccountMeta::new(account.pubkey(), false),
        ],
    );
    let txn = env.build_transaction(&[ix]);
    let result = env.execute_transaction(txn).await;
    assert!(
        result.is_ok(),
        "failed to execute schedule task transaction: {:?}",
        result
    );

    // Wait until we actually observe at least five executions
    let executed_before_cancel = tokio::time::timeout(
        Duration::from_millis(10 * interval as u64),
        async {
            loop {
                if let Some(value) =
                    env.get_account(account.pubkey()).data().first()
                {
                    if *value >= 5 {
                        break *value;
                    }
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        },
    )
    .await
    .expect("task scheduler never reached five executions within 10 intervals");

    // Cancel the task
    let ix = Instruction::new_with_bincode(
        guinea::ID,
        &GuineaInstruction::CancelTask(task_id),
        vec![
            AccountMeta::new_readonly(magicblock_magic_program_api::ID, false),
            AccountMeta::new(env.get_payer().pubkey, true),
        ],
    );
    let txn = env.build_transaction(&[ix]);
    let result = env.execute_transaction(txn).await;
    assert!(
        result.is_ok(),
        "failed to execute cancel task transaction: {:?}",
        result
    );

    // Wait for the cancel to be processed
    tokio::time::sleep(Duration::from_millis(interval as u64)).await;

    let value_at_cancel = env
        .get_account(account.pubkey())
        .data()
        .first()
        .copied()
        .unwrap_or_default();
    assert!(
            value_at_cancel >= executed_before_cancel,
            "unexpected: value at cancellation ({}) < value when 5 executions were observed ({})",
            value_at_cancel,
            executed_before_cancel
        );

    // Ensure the scheduler stops issuing executions after cancellation
    tokio::time::sleep(Duration::from_millis(2 * interval as u64)).await;

    let value_after_cancel = env
        .get_account(account.pubkey())
        .data()
        .first()
        .copied()
        .unwrap_or_default();

    assert_eq!(
        value_after_cancel, value_at_cancel,
        "task scheduler kept executing after cancellation"
    );

    token.cancel();
    handle.await.expect("task service join handle failed")?;

    Ok(())
}
