use magicblock_core::{
    intent::{CommitType, CommittedAccount, MagicIntentBundle},
    link::transactions::ReplayPosition,
};
use magicblock_magic_program_api::{
    instruction::MagicBlockInstruction, MAGIC_CONTEXT_PUBKEY,
    MAGIC_CONTEXT_SIZE,
};
use magicblock_program::{
    instruction_utils::InstructionUtils,
    magic_scheduled_base_intent::ScheduledIntentBundle,
    validator::{generate_validator_authority_if_needed, validator_authority},
    MagicContext, TransactionScheduler,
};
use solana_account::{
    Account, AccountSharedData, ReadableAccount, WritableAccount,
};
use solana_instruction::{error::InstructionError, AccountMeta, Instruction};
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::Transaction;
use solana_transaction_error::TransactionError;
use test_kit::ExecutionTestEnv;

fn stage_intents(
    env: &ExecutionTestEnv,
    ids: &[u64],
) -> Vec<ScheduledIntentBundle> {
    let bundles: Vec<_> = ids
        .iter()
        .map(|&id| ScheduledIntentBundle {
            id,
            slot: env.accountsdb.slot(),
            blockhash: env.ledger.latest_blockhash(),
            sent_transaction: Transaction::default(),
            payer: env.payers[0].pubkey(),
            intent_bundle: MagicIntentBundle {
                commit: Some(CommitType::Standalone(vec![CommittedAccount {
                    pubkey: Pubkey::new_unique(),
                    account: Account {
                        lamports: 123,
                        data: id.to_le_bytes().to_vec(),
                        owner: guinea::ID,
                        ..Default::default()
                    },
                    remote_slot: 456,
                }])),
                ..Default::default()
            },
        })
        .collect();
    let context = MagicContext {
        intent_id: ids.last().map_or(0, |id| id + 1),
        scheduled_base_intents: bundles.clone(),
    };
    let mut account = AccountSharedData::new(
        u64::MAX,
        MAGIC_CONTEXT_SIZE,
        &magicblock_magic_program_api::ID,
    );
    account.set_delegated(true);
    bincode::serialize_into(
        &mut &mut account.data_as_mut_slice()[..],
        &context,
    )
    .unwrap();
    env.accountsdb
        .insert_account(&MAGIC_CONTEXT_PUBKEY, &account)
        .unwrap();
    bundles
}

fn staged_intents(env: &ExecutionTestEnv) -> Vec<ScheduledIntentBundle> {
    let account = env.get_account(MAGIC_CONTEXT_PUBKEY);
    bincode::deserialize::<MagicContext>(account.data())
        .unwrap()
        .scheduled_base_intents
}

fn accept_instruction() -> Instruction {
    Instruction::new_with_bincode(
        magicblock_magic_program_api::ID,
        &MagicBlockInstruction::AcceptScheduleCommits,
        vec![
            AccountMeta::new_readonly(validator_authority().pubkey(), true),
            AccountMeta::new(MAGIC_CONTEXT_PUBKEY, false),
        ],
    )
}

fn acceptance_transaction(
    env: &ExecutionTestEnv,
    ixs: &[Instruction],
) -> Transaction {
    env.advance_slot();
    env.build_transaction_with_signers(ixs, &[&validator_authority()])
}

async fn assert_executor_has_no_pending_intents(env: &ExecutionTestEnv) {
    env.advance_slot();
    let txn = env.build_transaction(&[InstructionUtils::noop_instruction(0)]);
    env.execute_transaction(txn).await.unwrap();
    assert!(TransactionScheduler::default()
        .take_scheduled_intent_bundles()
        .is_empty());
}

async fn check_failed_acceptance_and_retry() {
    let env = ExecutionTestEnv::new();
    env.fund_account(validator_authority().pubkey(), 1_000_000);
    let expected = stage_intents(&env, &[1, 2]);
    let staged_data = env.get_account(MAGIC_CONTEXT_PUBKEY).data().to_vec();
    let fail = Instruction {
        program_id: guinea::ID,
        accounts: vec![],
        data: vec![],
    };
    let txn = acceptance_transaction(&env, &[accept_instruction(), fail]);
    let failed_blockhash = txn.message.recent_blockhash;
    assert_eq!(
        env.execute_transaction(txn).await,
        Err(TransactionError::InstructionError(
            1,
            InstructionError::InvalidInstructionData,
        ))
    );
    assert_eq!(env.get_account(MAGIC_CONTEXT_PUBKEY).data(), staged_data);
    assert_eq!(TransactionScheduler::default().scheduled_actions_len(), 0);
    assert_executor_has_no_pending_intents(&env).await;

    // Multiple accept instructions in one transaction must still publish once.
    let retry = acceptance_transaction(
        &env,
        &[accept_instruction(), accept_instruction()],
    );
    assert_ne!(retry.message.recent_blockhash, failed_blockhash);
    env.execute_transaction(retry).await.unwrap();
    assert!(staged_intents(&env).is_empty());
    assert_eq!(
        TransactionScheduler::default().take_scheduled_intent_bundles(),
        expected,
    );
    assert_executor_has_no_pending_intents(&env).await;
}

async fn check_simulated_acceptance() {
    let env = ExecutionTestEnv::new();
    env.fund_account(validator_authority().pubkey(), 1_000_000);
    let expected = stage_intents(&env, &[4, 5]);
    let staged_data = env.get_account(MAGIC_CONTEXT_PUBKEY).data().to_vec();
    let txn = acceptance_transaction(&env, &[accept_instruction()]);
    let simulated = env.simulate_transaction(txn).await;
    assert_eq!(simulated.result, Ok(()));
    let simulated_context = simulated
        .post_simulation_accounts
        .iter()
        .find(|(key, _)| key == &MAGIC_CONTEXT_PUBKEY)
        .unwrap();
    assert!(
        bincode::deserialize::<MagicContext>(simulated_context.1.data())
            .unwrap()
            .scheduled_base_intents
            .is_empty()
    );
    assert_eq!(env.get_account(MAGIC_CONTEXT_PUBKEY).data(), staged_data);
    assert_eq!(TransactionScheduler::default().scheduled_actions_len(), 0);
    assert_executor_has_no_pending_intents(&env).await;

    let txn = acceptance_transaction(&env, &[accept_instruction()]);
    env.execute_transaction(txn).await.unwrap();
    assert_eq!(
        TransactionScheduler::default().take_scheduled_intent_bundles(),
        expected,
    );
}

async fn check_replayed_acceptance(persist: bool) {
    let env = ExecutionTestEnv::new_replica_mode(1, false);
    env.fund_account(validator_authority().pubkey(), 1_000_000);
    stage_intents(&env, &[6, 7]);
    let txn = env.build_transaction_with_signers(
        &[accept_instruction()],
        &[&validator_authority()],
    );
    env.transaction_scheduler
        .replay(
            ReplayPosition {
                slot: env.accountsdb.slot(),
                index: 0,
                persist,
            },
            txn,
        )
        .await
        .unwrap();
    // Replay submission is fire-and-forget; use the ordered completion barrier.
    env.transaction_scheduler
        .wait_for_replay_drain()
        .await
        .unwrap();
    assert!(staged_intents(&env).is_empty());
    assert_eq!(TransactionScheduler::default().scheduled_actions_len(), 0);

    env.switch_to_primary_mode();
    env.wait_for_scheduler_ready().await;
    assert_executor_has_no_pending_intents(&env).await;
    let expected = stage_intents(&env, &[8]);
    let txn = acceptance_transaction(&env, &[accept_instruction()]);
    env.execute_transaction(txn).await.unwrap();
    assert_eq!(
        TransactionScheduler::default().take_scheduled_intent_bundles(),
        expected,
    );
}

async fn check_acceptance_authority() {
    let env = ExecutionTestEnv::new();
    env.fund_account(validator_authority().pubkey(), 1_000_000);
    let expected = stage_intents(&env, &[9]);
    for missing_signature in [false, true] {
        let mut ix = accept_instruction();
        let error = if missing_signature {
            ix.accounts[0].is_signer = false;
            InstructionError::MissingRequiredSignature
        } else {
            ix.accounts[0].pubkey = env.payers[0].pubkey();
            InstructionError::InvalidArgument
        };
        env.advance_slot();
        let txn = env.build_transaction(&[ix]);
        assert_eq!(
            env.execute_transaction(txn).await,
            Err(TransactionError::InstructionError(0, error)),
        );
        assert_eq!(staged_intents(&env), expected);
        assert_eq!(TransactionScheduler::default().scheduled_actions_len(), 0);
        assert_executor_has_no_pending_intents(&env).await;
    }
}

#[tokio::test]
async fn intent_acceptance_transaction_boundaries() {
    // These cases share process-global authority, coordination mode, and queue.
    // Keep them sequential, with one executor per environment to exercise TLS reuse.
    generate_validator_authority_if_needed();
    assert_eq!(TransactionScheduler::default().scheduled_actions_len(), 0);
    check_failed_acceptance_and_retry().await;
    check_simulated_acceptance().await;
    check_replayed_acceptance(false).await;
    check_replayed_acceptance(true).await;
    check_acceptance_authority().await;
}
