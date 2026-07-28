use guinea::GuineaInstruction;
use magicblock_accounts_db::traits::AccountsBank;
use magicblock_magic_program_api::{
    args::ScheduleTaskArgs, instruction::AccountCloneFields,
    EPHEMERAL_VAULT_PUBKEY, MAGIC_CONTEXT_PUBKEY, MAGIC_CONTEXT_SIZE,
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
use solana_transaction_error::TransactionError;
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
    let mut action_payer_account = env.get_account(action_payer);
    action_payer_account.set_delegated(true);
    env.accountsdb
        .insert_account(&action_payer, &action_payer_account)
        .unwrap();

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
async fn executor_rejects_unwritable_action_dependencies_atomically() {
    generate_validator_authority_if_needed();

    for undelegating in [false, true] {
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
        counter_account.set_undelegating(undelegating);
        counter_account.set_data_from_slice(&[0]);
        env.accountsdb
            .insert_account(&counter, &counter_account)
            .unwrap();

        let actions = vec![Instruction::new_with_bincode(
            guinea::ID,
            &GuineaInstruction::Increment,
            vec![AccountMeta::new(counter, false)],
        )];
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
        assert_eq!(
            env.execute_transaction(txn).await.unwrap_err(),
            TransactionError::InstructionError(
                1,
                solana_instruction::error::InstructionError::IllegalOwner,
            )
        );

        let target_account = env.get_account(target);
        assert!(!target_account.delegated(), "clone must roll back");
        assert!(
            target_account.data().is_empty(),
            "the cloned data must roll back with the rejected action"
        );
        let counter_account = env.get_account(counter);
        assert_eq!(counter_account.undelegating(), undelegating);
        assert_eq!(counter_account.data(), &[0]);
    }
}

/// A drained (zero-lamport, empty) but still-undelegating dependency must stay
/// rejected: emptiness must not let it slip through the not-yet-created
/// carve-out and bypass the undelegating lock.
#[tokio::test]
async fn executor_rejects_empty_undelegating_action_dependency() {
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

    // Drained (zero lamports, empty data) but still undelegating and present.
    let counter = Pubkey::new_unique();
    let mut counter_account = AccountSharedData::new(0, 0, &guinea::ID);
    counter_account.set_undelegating(true);
    env.accountsdb
        .insert_account(&counter, &counter_account)
        .unwrap();
    assert!(
        env.get_account(counter).undelegating(),
        "drained undelegating account must persist in AccountsDb"
    );

    let actions = vec![Instruction::new_with_bincode(
        guinea::ID,
        &GuineaInstruction::Increment,
        vec![AccountMeta::new(counter, false)],
    )];
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
    assert_eq!(
        env.execute_transaction(txn).await.unwrap_err(),
        TransactionError::InstructionError(
            1,
            solana_instruction::error::InstructionError::IllegalOwner,
        )
    );

    let target_account = env.get_account(target);
    assert!(!target_account.delegated(), "clone must roll back");
    assert!(target_account.data().is_empty());
    let counter_account = env.get_account(counter);
    assert!(counter_account.undelegating(), "the lock must be preserved");
}

/// Actions legitimately create accounts mid-flight (receipts, permission PDAs,
/// rent-pending ATAs) via Magic CPIs, declaring them writable non-signer and
/// authorizing them with a program (PDA) signature. Such not-yet-created
/// accounts must pass the pre-invocation check; post-execution validation
/// still governs the result.
#[tokio::test]
async fn executor_allows_writable_action_account_created_by_the_action() {
    generate_validator_authority_if_needed();
    let env = ExecutionTestEnv::new_with_config(0, 1, false);
    let validator = validator_authority();
    env.fund_account(validator.pubkey(), 10_000_000);

    // Ephemeral vault, flagged the way the validator initializes it.
    env.fund_account_with_owner(
        EPHEMERAL_VAULT_PUBKEY,
        1_000_000,
        magicblock_magic_program_api::ID,
    );
    let mut vault_account = env.get_account(EPHEMERAL_VAULT_PUBKEY);
    vault_account.set_ephemeral(true);
    env.accountsdb
        .insert_account(&EPHEMERAL_VAULT_PUBKEY, &vault_account)
        .unwrap();

    let target = Pubkey::new_unique();
    env.accountsdb
        .insert_account(
            &target,
            &AccountSharedData::new(100, 0, &system_program::id()),
        )
        .unwrap();

    // Delegated sponsor paying the ephemeral rent from inside the action.
    let sponsor = Pubkey::new_unique();
    env.fund_account(sponsor, 100_000_000);
    let mut sponsor_account = env.get_account(sponsor);
    sponsor_account.set_delegated(true);
    env.accountsdb
        .insert_account(&sponsor, &sponsor_account)
        .unwrap();

    // A PDA of the invoked program, not yet created, declared writable
    // non-signer; the program authorizes it by signing the Magic CPI with seeds.
    let (receipt, _) = guinea::ephemeral_pda();
    let actions = vec![Instruction::new_with_bincode(
        guinea::ID,
        &GuineaInstruction::CreateEphemeralPdaAccount { data_len: 8 },
        vec![
            AccountMeta::new_readonly(magicblock_magic_program_api::ID, false),
            AccountMeta::new(sponsor, true),
            AccountMeta::new(receipt, false),
            AccountMeta::new(EPHEMERAL_VAULT_PUBKEY, false),
        ],
    )];

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

    let receipt_account = env.get_account(receipt);
    assert!(
        receipt_account.ephemeral(),
        "the action must create the receipt as an ephemeral account"
    );
    assert_eq!(receipt_account.data().len(), 8);

    let target_account = env.get_account(target);
    assert!(target_account.delegated());
    assert_eq!(target_account.data(), &[9]);
}

/// A nonexistent account declared writable AND signer must stay rejected:
/// action signers are synthesized without signatures, so an absent signer
/// could satisfy a program's signer check and squat any unused pubkey.
#[tokio::test]
async fn executor_rejects_nonexistent_writable_signer_action_account() {
    generate_validator_authority_if_needed();
    let env = ExecutionTestEnv::new_with_config(0, 1, false);
    let validator = validator_authority();
    env.fund_account(validator.pubkey(), 10_000_000);

    env.fund_account_with_owner(
        EPHEMERAL_VAULT_PUBKEY,
        1_000_000,
        magicblock_magic_program_api::ID,
    );
    let mut vault_account = env.get_account(EPHEMERAL_VAULT_PUBKEY);
    vault_account.set_ephemeral(true);
    env.accountsdb
        .insert_account(&EPHEMERAL_VAULT_PUBKEY, &vault_account)
        .unwrap();

    let target = Pubkey::new_unique();
    env.accountsdb
        .insert_account(
            &target,
            &AccountSharedData::new(100, 0, &system_program::id()),
        )
        .unwrap();

    let sponsor = Pubkey::new_unique();
    env.fund_account(sponsor, 100_000_000);
    let mut sponsor_account = env.get_account(sponsor);
    sponsor_account.set_delegated(true);
    env.accountsdb
        .insert_account(&sponsor, &sponsor_account)
        .unwrap();

    // No keypair exists for this pubkey; the action asks for it as a
    // writable signer to squat it via the keypair-based creation path.
    let squatted = Pubkey::new_unique();
    let actions = vec![Instruction::new_with_bincode(
        guinea::ID,
        &GuineaInstruction::CreateEphemeralAccount { data_len: 8 },
        vec![
            AccountMeta::new_readonly(magicblock_magic_program_api::ID, false),
            AccountMeta::new(sponsor, true),
            AccountMeta::new(squatted, true),
            AccountMeta::new(EPHEMERAL_VAULT_PUBKEY, false),
        ],
    )];

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
    assert_eq!(
        env.execute_transaction(txn).await.unwrap_err(),
        TransactionError::InstructionError(
            1,
            solana_instruction::error::InstructionError::IllegalOwner,
        )
    );

    assert!(
        env.accountsdb.get_account(&squatted).is_none(),
        "the squatted pubkey must not be created"
    );
    let target_account = env.get_account(target);
    assert!(!target_account.delegated(), "clone must roll back");
}

/// Duplicate-meta squat: the absent pubkey appears once writable non-signer and
/// once read-only signer. Signer authority is keyed by pubkey, so a per-meta
/// check is insufficient — the carve-out must gate on the whole signer set.
#[tokio::test]
async fn executor_rejects_squat_via_duplicate_writable_and_signer_metas() {
    generate_validator_authority_if_needed();
    let env = ExecutionTestEnv::new_with_config(0, 1, false);
    let validator = validator_authority();
    env.fund_account(validator.pubkey(), 10_000_000);

    env.fund_account_with_owner(
        EPHEMERAL_VAULT_PUBKEY,
        1_000_000,
        magicblock_magic_program_api::ID,
    );
    let mut vault_account = env.get_account(EPHEMERAL_VAULT_PUBKEY);
    vault_account.set_ephemeral(true);
    env.accountsdb
        .insert_account(&EPHEMERAL_VAULT_PUBKEY, &vault_account)
        .unwrap();

    let target = Pubkey::new_unique();
    env.accountsdb
        .insert_account(
            &target,
            &AccountSharedData::new(100, 0, &system_program::id()),
        )
        .unwrap();

    let sponsor = Pubkey::new_unique();
    env.fund_account(sponsor, 100_000_000);
    let mut sponsor_account = env.get_account(sponsor);
    sponsor_account.set_delegated(true);
    env.accountsdb
        .insert_account(&sponsor, &sponsor_account)
        .unwrap();

    // `squatted` is the positional ephemeral account (writable, non-signer) so
    // it dodges a per-meta signer check, while a trailing duplicate meta marks
    // the same pubkey a signer so `native_invoke` grants it signer authority.
    let squatted = Pubkey::new_unique();
    let actions = vec![Instruction::new_with_bincode(
        guinea::ID,
        &GuineaInstruction::CreateEphemeralAccount { data_len: 8 },
        vec![
            AccountMeta::new_readonly(magicblock_magic_program_api::ID, false),
            AccountMeta::new(sponsor, true),
            AccountMeta::new(squatted, false),
            AccountMeta::new(EPHEMERAL_VAULT_PUBKEY, false),
            AccountMeta::new_readonly(squatted, true),
        ],
    )];

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
    assert_eq!(
        env.execute_transaction(txn).await.unwrap_err(),
        TransactionError::InstructionError(
            1,
            solana_instruction::error::InstructionError::IllegalOwner,
        )
    );

    assert!(
        env.accountsdb.get_account(&squatted).is_none(),
        "the squatted pubkey must not be created"
    );
    let target_account = env.get_account(target);
    assert!(!target_account.delegated(), "clone must roll back");
}

/// If the action declares a nonexistent account writable but never creates it,
/// post-execution writable validation must still fail the transaction and roll
/// back the clone.
#[tokio::test]
async fn nonexistent_writable_action_account_left_uncreated_still_rolls_back() {
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

    // Declared writable, but the action never creates it.
    let phantom = Pubkey::new_unique();
    let actions = vec![Instruction::new_with_bincode(
        guinea::ID,
        &GuineaInstruction::ComputeBalances,
        vec![AccountMeta::new(phantom, false)],
    )];

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
    assert_eq!(
        env.execute_transaction(txn).await.unwrap_err(),
        TransactionError::InvalidWritableAccount,
    );

    let target_account = env.get_account(target);
    assert!(!target_account.delegated(), "clone must roll back");
    assert!(
        target_account.data().is_empty(),
        "the cloned data must roll back with the rejected action"
    );
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
