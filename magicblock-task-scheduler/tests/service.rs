use std::time::Duration;

use guinea::GuineaInstruction;
use magicblock_config::TaskSchedulerConfig;
use magicblock_program::{
    args::ScheduleTaskArgs, validator::init_validator_authority,
};
use magicblock_task_scheduler::{
    errors::TaskSchedulerResult, SchedulerDatabase, TaskSchedulerService,
};
use solana_account::ReadableAccount;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    native_token::LAMPORTS_PER_SOL,
};
use test_kit::{ExecutionTestEnv, Signer};
use tokio_util::sync::CancellationToken;

#[tokio::test]
pub async fn test_schedule_task() -> TaskSchedulerResult<()> {
    let mut env = ExecutionTestEnv::new();
    let account =
        env.create_account_with_config(LAMPORTS_PER_SOL, 1, guinea::ID);

    init_validator_authority(env.payer.insecure_clone());

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
    .start()?;

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
            AccountMeta::new(env.payer.pubkey(), true),
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

    // Wait the task scheduler to receive the task
    tokio::time::sleep(Duration::from_millis(10)).await;

    assert_eq!(
        env.get_account(account.pubkey()).data().first(),
        Some(&1),
        "the first byte of the account data should have been modified"
    );

    token.cancel();
    handle.await.unwrap().unwrap();

    Ok(())
}

#[tokio::test]
pub async fn test_cancel_task() -> TaskSchedulerResult<()> {
    let mut env = ExecutionTestEnv::new();
    let account =
        env.create_account_with_config(LAMPORTS_PER_SOL, 1, guinea::ID);

    init_validator_authority(env.payer.insecure_clone());

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
    .start()?;

    // Schedule a task
    let interval = 100;
    let ix = Instruction::new_with_bincode(
        guinea::ID,
        &GuineaInstruction::ScheduleTask(ScheduleTaskArgs {
            task_id: 1,
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
            AccountMeta::new(env.payer.pubkey(), true),
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

    // Wait the task scheduler to execute 10 times (+some to register the task)
    tokio::time::sleep(Duration::from_millis(5 * interval)).await;

    // Cancel the task
    let ix = Instruction::new_with_bincode(
        guinea::ID,
        &GuineaInstruction::CancelTask(1),
        vec![
            AccountMeta::new_readonly(magicblock_magic_program_api::ID, false),
            AccountMeta::new(env.payer.pubkey(), true),
        ],
    );
    let txn = env.build_transaction(&[ix]);
    let result = env.execute_transaction(txn).await;
    assert!(
        result.is_ok(),
        "failed to execute cancel task transaction: {:?}",
        result
    );

    // Wait the task scheduler to cancel the task and make sure it didn't execute again
    tokio::time::sleep(Duration::from_millis(2 * interval)).await;

    assert_eq!(
        env.get_account(account.pubkey()).data().first(),
        Some(&5),
        "the first byte of the account data should have been modified"
    );

    token.cancel();
    handle.await.unwrap().unwrap();

    Ok(())
}
