use guinea::GuineaInstruction;
use magicblock_magic_program_api::{
    args::ScheduleTaskArgs, instruction::AccountCloneFields,
    MAGIC_CONTEXT_PUBKEY, MAGIC_CONTEXT_SIZE,
};
use magicblock_program::{
    instruction_utils::InstructionUtils,
    validator::{generate_validator_authority_if_needed, validator_authority},
    MagicContext,
};
use solana_account::{AccountSharedData, ReadableAccount};
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;
use solana_sdk_ids::system_program;
use solana_signer::Signer;
use test_kit::ExecutionTestEnv;

/// Inserts the magic context account so that scheduling instructions which
/// write scheduled actions into it can execute against the runtime.
fn insert_magic_context(env: &ExecutionTestEnv) {
    let mut magic_context = AccountSharedData::new(
        u64::MAX,
        MAGIC_CONTEXT_SIZE,
        &magicblock_magic_program_api::ID,
    );
    magic_context.set_delegated(true);
    env.accountsdb
        .insert_account(&MAGIC_CONTEXT_PUBKEY, &magic_context)
        .unwrap();
}

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

#[tokio::test]
async fn schedule_undelegation_marks_cloned_account_as_undelegated() {
    generate_validator_authority_if_needed();
    let env = ExecutionTestEnv::new_with_config(0, 1, false);
    let validator = validator_authority();
    env.fund_account(validator.pubkey(), 10_000_000);
    insert_magic_context(&env);

    // The account being rescued starts as a plain remote account.
    let target = Pubkey::new_unique();
    env.accountsdb
        .insert_account(
            &target,
            &AccountSharedData::new(100, 0, &system_program::id()),
        )
        .unwrap();

    // Clone it as a delegated account, then schedule its undelegation in the
    // same transaction (mirrors the cloner's small-account rescue path).
    let clone_ix = InstructionUtils::clone_account_instruction(
        target,
        vec![7],
        AccountCloneFields {
            lamports: 1_000_000,
            owner: system_program::id(),
            delegated: true,
            remote_slot: 1,
            ..Default::default()
        },
        Vec::new(),
    );
    let schedule_ix =
        InstructionUtils::schedule_cloned_account_undelegation_instruction(
            target,
        );

    let txn = env.build_transaction_with_signers(
        &[clone_ix, schedule_ix],
        &[&validator],
    );
    env.execute_transaction(txn).await.unwrap();

    // The schedule instruction mutates the cloned account's owner/state, so it
    // must be writable in the instruction. After execution the account is
    // marked undelegating and no longer delegated.
    let target_account = env.get_account(target);
    assert!(target_account.undelegating());
    assert!(!target_account.delegated());
}

#[tokio::test]
async fn schedule_undelegation_commits_original_owner() {
    generate_validator_authority_if_needed();
    let env = ExecutionTestEnv::new_with_config(0, 1, false);
    let validator = validator_authority();
    env.fund_account(validator.pubkey(), 10_000_000);
    insert_magic_context(&env);

    let target = Pubkey::new_unique();
    env.accountsdb
        .insert_account(
            &target,
            &AccountSharedData::new(100, 0, &system_program::id()),
        )
        .unwrap();

    // Clone a delegated account owned by a real program, then schedule its
    // undelegation in the same transaction.
    let clone_ix = InstructionUtils::clone_account_instruction(
        target,
        vec![7],
        AccountCloneFields {
            lamports: 1_000_000,
            owner: guinea::ID,
            delegated: true,
            remote_slot: 1,
            ..Default::default()
        },
        Vec::new(),
    );
    let schedule_ix =
        InstructionUtils::schedule_cloned_account_undelegation_instruction(
            target,
        );

    let txn = env.build_transaction_with_signers(
        &[clone_ix, schedule_ix],
        &[&validator],
    );
    env.execute_transaction(txn).await.unwrap();

    // The scheduled intent must commit the account with its original owner.
    // Scheduling marks the live account as undelegating (owner flipped to the
    // delegation program); on mmap-backed accounts that mutation is visible
    // through shallow snapshots, so a wrong ordering in the processor bakes
    // the delegation program in as the owner. The committor then derives the
    // dlp program-config PDA from it and the base-layer commit is rejected
    // with InvalidAuthority.
    let context_data = env.get_account(MAGIC_CONTEXT_PUBKEY);
    let context: MagicContext =
        bincode::deserialize(context_data.data()).unwrap();
    let intent = context
        .scheduled_base_intents
        .first()
        .expect("undelegation intent must be scheduled");
    let committed = intent
        .intent_bundle
        .commit_and_undelegate
        .as_ref()
        .expect("intent must be a commit-and-undelegate")
        .get_committed_accounts()
        .first()
        .expect("intent must commit the cloned account");
    assert_eq!(committed.pubkey, target);
    assert_eq!(committed.account.owner, guinea::ID);

    // The live account is still locked for undelegation as before.
    let target_account = env.get_account(target);
    assert!(target_account.undelegating());
    assert!(!target_account.delegated());
}

#[tokio::test]
async fn chunked_rescue_undelegation_clears_pending_clone() {
    generate_validator_authority_if_needed();
    let env = ExecutionTestEnv::new_with_config(0, 1, false);
    let validator = validator_authority();
    env.fund_account(validator.pubkey(), 10_000_000);
    insert_magic_context(&env);

    let target = Pubkey::new_unique();
    env.accountsdb
        .insert_account(
            &target,
            &AccountSharedData::new(100, 0, &system_program::id()),
        )
        .unwrap();

    let fields = AccountCloneFields {
        lamports: 1_000_000,
        owner: system_program::id(),
        delegated: true,
        remote_slot: 1,
        ..Default::default()
    };

    // 1. Initialize a chunked (large-account) clone. This registers the pubkey
    //    in the process-global PENDING_CLONES set.
    let init_ix = InstructionUtils::clone_account_init_instruction(
        target,
        1,
        vec![1],
        fields,
    );
    let init_tx = env.build_transaction_with_signers(&[init_ix], &[&validator]);
    env.execute_transaction(init_tx).await.unwrap();

    // 2. Final chunk requesting undelegation, paired with the schedule
    //    instruction. The final `CloneAccountContinue(needs_undelegation=true)`
    //    intentionally leaves the clone pending so the sibling schedule
    //    instruction can validate the previous clone; the schedule instruction
    //    must then clear the pending entry.
    let continue_ix = InstructionUtils::clone_account_continue_instruction(
        target,
        1,
        Vec::new(),
        true,
        Vec::new(),
        true,
    );
    let schedule_ix =
        InstructionUtils::schedule_cloned_account_undelegation_instruction(
            target,
        );
    let rescue_tx = env.build_transaction_with_signers(
        &[continue_ix, schedule_ix],
        &[&validator],
    );

    // Before the rescue, the chunked clone is still pending (the final continue
    // intentionally leaves it so the schedule instruction can validate it).
    assert!(magicblock_program::is_pending_clone(&target));

    env.execute_transaction(rescue_tx).await.unwrap();
    assert!(env.get_account(target).undelegating());
    assert!(!env.get_account(target).delegated());

    // The schedule instruction must clear the process-global pending-clone
    // entry; otherwise a later clone init fails with CloneAlreadyPending and
    // cleanup could close already-completed state.
    assert!(!magicblock_program::is_pending_clone(&target));
}
