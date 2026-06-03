use guinea::GuineaInstruction;
use magicblock_magic_program_api::{
    args::ScheduleTaskArgs, instruction::AccountCloneFields,
};
use magicblock_program::{
    instruction_utils::InstructionUtils,
    validator::{generate_validator_authority_if_needed, validator_authority},
};
use solana_account::{AccountSharedData, ReadableAccount};
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;
use solana_sdk_ids::system_program;
use solana_signer::Signer;
use test_kit::ExecutionTestEnv;

#[tokio::test]
async fn executor_runs_post_delegation_actions_after_clone() {
    generate_validator_authority_if_needed();
    let env = ExecutionTestEnv::new_with_config(0, 1, false);
    let validator = validator_authority();
    env.fund_account(validator.pubkey(), 10_000_000);

    let target = Pubkey::new_unique();
    env.accountsdb
        .insert_account(
            &target,
            &AccountSharedData::new(100, 0, &system_program::id()),
        )
        .unwrap();

    let counter = Pubkey::new_unique();
    let mut counter_account = AccountSharedData::new(1_000, 1, &guinea::ID);
    counter_account.set_delegated(true);
    counter_account.set_data_from_slice(&[0]);
    env.accountsdb
        .insert_account(&counter, &counter_account)
        .unwrap();

    let action_payer = Pubkey::new_unique();
    env.fund_account(action_payer, 1_000_000);

    let schedule_task_action = Instruction::new_with_bincode(
        guinea::ID,
        &GuineaInstruction::ScheduleTask(ScheduleTaskArgs {
            task_id: 1,
            execution_interval_millis: 1,
            iterations: 1,
            instructions: vec![InstructionUtils::noop_instruction(0)],
        }),
        vec![
            AccountMeta::new_readonly(magicblock_magic_program_api::ID, false),
            AccountMeta::new(action_payer, true),
            AccountMeta::new(counter, false),
        ],
    );
    let increment_action = Instruction::new_with_bincode(
        guinea::ID,
        &GuineaInstruction::Increment,
        vec![AccountMeta::new(counter, false)],
    );
    let actions = vec![schedule_task_action, increment_action];

    let clone_ix = InstructionUtils::clone_account_instruction(
        target,
        vec![9],
        AccountCloneFields {
            lamports: 1_000_000,
            owner: system_program::id(),
            delegated: true,
            remote_slot: 1,
            ..Default::default()
        },
        actions.clone(),
    );
    let executor_ix =
        InstructionUtils::post_delegation_action_executor_instruction(
            target, actions,
        );

    let txn = env.build_transaction_with_signers(
        &[clone_ix, executor_ix],
        &[&validator],
    );
    env.execute_transaction(txn).await.unwrap();

    let target_account = env.get_account(target);
    assert!(target_account.delegated());
    assert_eq!(target_account.data(), &[9]);

    let counter_account = env.get_account(counter);
    assert_eq!(counter_account.data(), &[1]);
}
