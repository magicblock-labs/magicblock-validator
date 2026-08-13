use std::{collections::HashMap, sync::Arc};

use borsh::to_vec;
use magicblock_committor_service::{
    committor_processor::CommittorProcessor,
    config::ChainConfig,
    intent_executor::{error::IntentExecutorError, ExecutionOutput},
    persist::CommitStrategy,
    ComputeBudgetConfig,
};
use magicblock_core::intent::{
    types::CommittedAccount, CommitAndUndelegate, CommitType, MagicBaseIntent,
    MagicIntentBundle, UndelegateType,
};
use magicblock_program::magic_scheduled_base_intent::ScheduledIntentBundle;
use program_flexi_counter::state::FlexiCounter;
use solana_account::{Account, ReadableAccount};
use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    hash::Hash, signature::Keypair, signer::Signer, transaction::Transaction,
};
use test_kit::init_logger;
use tokio::task::JoinSet;
use tracing::*;
use utils::transactions::print_tx_logs;

use self::utils::transactions::init_and_delegate_order_book_on_chain;
use crate::utils::{
    ensure_validator_authority,
    transactions::{
        fund_validator_auth_and_ensure_validator_fees_vault,
        init_and_delegate_account_on_chain,
    },
};

mod common;
mod utils;

// -----------------
// Utilities and Setup
// -----------------
type ExpectedStrategies = HashMap<CommitStrategy, u8>;

///
/// Unlike ScheduleCommitType which always implies Finalize (because that
/// simulates "user-facing" schedule commit intent), CommitIntentKind simulates
/// the explicit committor intent and therefore its members do NOT imply
/// Finalize by default.
///
#[derive(Clone, Copy, Debug)]
enum CommitIntentKind {
    Commit,
    CommitAndUndelegate,
    CommitFinalize,
    CommitFinalizeAndUndelegate,
}

fn expect_strategies(
    strategies: &[(CommitStrategy, u8)],
) -> ExpectedStrategies {
    let mut expected_strategies = HashMap::new();
    for (strategy, count) in strategies {
        *expected_strategies.entry(*strategy).or_insert(0) += count;
    }
    expected_strategies
}

// -----------------
// +++++ Tests +++++
// -----------------

// -----------------
// Single Account Commits
// -----------------

#[tokio::test]
async fn test_ix_commit_single_account_100_bytes() {
    commit_single_account(
        100,
        CommitStrategy::StateArgs,
        CommitIntentKind::Commit,
    )
    .await;
}

#[tokio::test]
async fn test_ix_commit_single_account_100_bytes_and_undelegate() {
    commit_single_account(
        100,
        CommitStrategy::StateArgs,
        CommitIntentKind::CommitAndUndelegate,
    )
    .await;
}

#[tokio::test]
async fn test_ix_commit_single_account_256_bytes() {
    commit_single_account(
        256,
        CommitStrategy::StateArgs,
        CommitIntentKind::Commit,
    )
    .await;
}

#[tokio::test]
async fn test_ix_commit_single_account_257_bytes() {
    commit_single_account(
        257,
        CommitStrategy::DiffArgs,
        CommitIntentKind::Commit,
    )
    .await;
}

#[tokio::test]
async fn test_ix_commit_single_account_256_bytes_and_undelegate() {
    commit_single_account(
        256,
        CommitStrategy::StateArgs,
        CommitIntentKind::CommitAndUndelegate,
    )
    .await;
}

#[tokio::test]
async fn test_ix_commit_single_account_257_bytes_and_undelegate() {
    commit_single_account(
        257,
        CommitStrategy::DiffArgs,
        CommitIntentKind::CommitAndUndelegate,
    )
    .await;
}

#[tokio::test]
async fn test_ix_commit_single_account_800_bytes() {
    commit_single_account(
        800,
        CommitStrategy::DiffArgs,
        CommitIntentKind::Commit,
    )
    .await;
}

#[tokio::test]
async fn test_ix_commit_single_account_800_bytes_and_undelegate() {
    commit_single_account(
        800,
        CommitStrategy::DiffArgs,
        CommitIntentKind::CommitAndUndelegate,
    )
    .await;
}

#[tokio::test]
async fn test_ix_commit_single_account_one_kb() {
    commit_single_account(
        1024,
        CommitStrategy::DiffArgs,
        CommitIntentKind::Commit,
    )
    .await;
}

#[tokio::test]
async fn test_ix_commit_single_account_ten_kb() {
    commit_single_account(
        10 * 1024,
        CommitStrategy::DiffArgs,
        CommitIntentKind::Commit,
    )
    .await;
}

#[tokio::test]
async fn test_ix_commit_order_book_change_100_bytes() {
    commit_book_order_account(
        100,
        CommitStrategy::DiffArgs,
        CommitIntentKind::Commit,
    )
    .await;
}

#[tokio::test]
async fn test_ix_commit_order_book_change_636_bytes() {
    commit_book_order_account(
        636,
        CommitStrategy::DiffArgs,
        CommitIntentKind::Commit,
    )
    .await;
}

#[tokio::test]
async fn test_ix_commit_order_book_change_637_bytes() {
    // 636 bytes still produces a raw tx within the 1232-byte packet limit
    // (including the first-commit uniqueness noop). 637 bytes crosses it
    // by one byte.
    commit_book_order_account(
        637,
        CommitStrategy::DiffBuffer,
        CommitIntentKind::Commit,
    )
    .await;
}

#[tokio::test]
async fn test_ix_commit_order_book_change_10k_bytes() {
    commit_book_order_account(
        10 * 1024,
        CommitStrategy::DiffBuffer,
        CommitIntentKind::CommitAndUndelegate,
    )
    .await;
}

#[tokio::test]
async fn test_ix_commit_finalize_order_book_change_10k_bytes() {
    commit_book_order_account(
        10 * 1024,
        CommitStrategy::DiffBuffer,
        CommitIntentKind::CommitFinalize,
    )
    .await;
}

async fn commit_single_account(
    bytes: usize,
    expected_strategy: CommitStrategy,
    commit_type: CommitIntentKind,
) {
    init_logger!();

    let validator_auth = ensure_validator_authority();
    fund_validator_auth_and_ensure_validator_fees_vault(&validator_auth).await;

    // Run each test with and without finalizing
    let processor = Arc::new(
        CommittorProcessor::try_new(
            validator_auth.insecure_clone(),
            ":memory:",
            ChainConfig::local(ComputeBudgetConfig::new(1_000_000)),
            None,
            common::MockActionsCallbackExecutor::default(),
        )
        .unwrap(),
    );

    let counter_auth = Keypair::new();
    let (pubkey, mut account) =
        init_and_delegate_account_on_chain(&counter_auth, bytes as u64, None)
            .await;

    let counter = FlexiCounter {
        label: "Counter".to_string(),
        updates: 0,
        count: 101,
    };
    let mut data = to_vec(&counter).unwrap();
    data.resize(bytes, 0);
    account.data = data;
    account.owner = program_flexi_counter::id();

    let account = CommittedAccount {
        pubkey,
        account,
        remote_slot: Default::default(),
    };
    let base_intent = match commit_type {
        CommitIntentKind::Commit => {
            MagicBaseIntent::Commit(CommitType::Standalone(vec![account]))
        }
        CommitIntentKind::CommitAndUndelegate => {
            MagicBaseIntent::CommitAndUndelegate(CommitAndUndelegate {
                commit_action: CommitType::Standalone(vec![account]),
                undelegate_action: UndelegateType::Standalone,
            })
        }
        CommitIntentKind::CommitFinalize => {
            MagicBaseIntent::CommitFinalize(CommitType::Standalone(vec![
                account,
            ]))
        }
        CommitIntentKind::CommitFinalizeAndUndelegate => {
            MagicBaseIntent::CommitFinalizeAndUndelegate(CommitAndUndelegate {
                commit_action: CommitType::Standalone(vec![account]),
                undelegate_action: UndelegateType::Standalone,
            })
        }
    };

    let intent = ScheduledIntentBundle {
        id: 0,
        slot: 10,
        blockhash: Hash::new_unique(),
        sent_transaction: Transaction::default(),
        payer: counter_auth.pubkey(),
        intent_bundle: base_intent.into(),
    };

    // We should always be able to Commit & Finalize 1 account either with Args or Buffers
    ix_commit_local(
        processor,
        vec![intent],
        expect_strategies(&[(expected_strategy, 1)]),
        program_flexi_counter::ID,
    )
    .await;
}

async fn commit_book_order_account(
    changed_len: usize,
    expected_strategy: CommitStrategy,
    commit_type: CommitIntentKind,
) {
    init_logger!();

    let validator_auth = ensure_validator_authority();
    fund_validator_auth_and_ensure_validator_fees_vault(&validator_auth).await;

    // Run each test with and without finalizing
    let processor = Arc::new(
        CommittorProcessor::try_new(
            validator_auth.insecure_clone(),
            ":memory:",
            ChainConfig::local(ComputeBudgetConfig::new(1_000_000)),
            None,
            common::MockActionsCallbackExecutor::default(),
        )
        .unwrap(),
    );

    let payer = Keypair::new();
    let (order_book_pk, mut order_book_ac) =
        init_and_delegate_order_book_on_chain(&payer).await;

    // Modify bytes so that a diff is produced and is sent to DLP
    let data = &mut order_book_ac.data;
    assert!(changed_len <= data.len());
    for byte in &mut order_book_ac.data[..changed_len] {
        *byte = byte.wrapping_add(1);
    }
    order_book_ac.owner = program_schedulecommit::id();

    // We should always be able to Commit & Finalize 1 account either with Args or Buffers
    let account = CommittedAccount {
        pubkey: order_book_pk,
        account: order_book_ac,
        remote_slot: Default::default(),
    };
    let base_intent = match commit_type {
        CommitIntentKind::Commit => {
            MagicBaseIntent::Commit(CommitType::Standalone(vec![account]))
        }
        CommitIntentKind::CommitAndUndelegate => {
            MagicBaseIntent::CommitAndUndelegate(CommitAndUndelegate {
                commit_action: CommitType::Standalone(vec![account]),
                undelegate_action: UndelegateType::Standalone,
            })
        }
        CommitIntentKind::CommitFinalize => {
            MagicBaseIntent::CommitFinalize(CommitType::Standalone(vec![
                account,
            ]))
        }
        CommitIntentKind::CommitFinalizeAndUndelegate => {
            MagicBaseIntent::CommitFinalizeAndUndelegate(CommitAndUndelegate {
                commit_action: CommitType::Standalone(vec![account]),
                undelegate_action: UndelegateType::Standalone,
            })
        }
    };

    let intent = ScheduledIntentBundle {
        id: 0,
        slot: 10,
        blockhash: Hash::new_unique(),
        sent_transaction: Transaction::default(),
        payer: payer.pubkey(),
        intent_bundle: base_intent.into(),
    };

    ix_commit_local(
        processor,
        vec![intent],
        expect_strategies(&[(expected_strategy, 1)]),
        program_schedulecommit::ID,
    )
    .await;
}

// -----------------
// Oversized Commits (PreallocateBuffer)
// -----------------
//
// Accounts whose committed state exceeds MAX_PERMITTED_DATA_INCREASE
// (10_240 bytes) can no longer grow their commit_state PDA (or, on
// finalize, the delegated account itself) in a single instruction, so
// preparation sends PreallocateBuffer instructions ahead of the actual
// commit/finalize to grow those accounts in steps first.

#[tokio::test]
async fn test_ix_commit_single_account_50kb_buffer() {
    commit_large_account(
        50 * 1024,
        CommitStrategy::DiffBuffer,
        CommitIntentKind::Commit,
    )
    .await;
}

#[tokio::test]
async fn test_ix_commit_finalize_single_account_350kb_buffer() {
    // 350KB needs more preallocate steps than fit in one transaction
    // (350 * 1024 / MAX_PERMITTED_DATA_INCREASE > PREALLOCATE_CHUNK_SIZE),
    // so this also exercises preallocate chunking across multiple txs, for
    // both the commit_state PDA (commit) and the delegated account (finalize).
    commit_large_account(
        350 * 1024,
        CommitStrategy::DiffBuffer,
        CommitIntentKind::CommitFinalize,
    )
    .await;
}

#[tokio::test]
async fn test_ix_commit_and_undelegate_single_account_50kb_buffer() {
    // Two-stage: commit (PreallocateCommitStateTask grows commit_state PDA)
    // then finalize + undelegate (PreallocateFinalizeTask grows the
    // delegated account itself, PreallocateUndelegateTask grows the
    // undelegate buffer) -- exercises all three PreallocateBufferKind paths.
    commit_large_account(
        50 * 1024,
        CommitStrategy::DiffBuffer,
        CommitIntentKind::CommitAndUndelegate,
    )
    .await;
}

#[tokio::test]
async fn test_ix_commit_finalize_and_undelegate_single_account_50kb_buffer() {
    // Single combined commit+finalize (PreallocateCommitFinalizeTask grows
    // the delegated account directly) followed by undelegate
    // (PreallocateUndelegateTask grows the undelegate buffer).
    commit_large_account(
        50 * 1024,
        CommitStrategy::DiffBuffer,
        CommitIntentKind::CommitFinalizeAndUndelegate,
    )
    .await;
}

/// Delegates a *small* account, then commits (and optionally finalizes) it
/// with `bytes` of brand new random data -- far more than what's currently
/// delegated. The delegated account starts small deliberately: Solana's own
/// realloc cap makes it impossible to delegate an account that's already
/// bigger than MAX_PERMITTED_DATA_INCREASE in one shot, so PreallocateBuffer
/// only ever needs to grow accounts *after* they're already delegated --
/// exactly this shape.
///
/// The random data (vs. only a few changed fields) guarantees a large diff
/// against the small base state, forcing buffer delivery: a small diff,
/// however large the account grows, could still fit inline as
/// [`CommitStrategy::DiffArgs`].
async fn commit_large_account(
    bytes: usize,
    expected_strategy: CommitStrategy,
    commit_type: CommitIntentKind,
) {
    const DELEGATED_ACCOUNT_SIZE: u64 = 100;

    init_logger!();

    let validator_auth = ensure_validator_authority();
    fund_validator_auth_and_ensure_validator_fees_vault(&validator_auth).await;

    let processor = Arc::new(
        CommittorProcessor::try_new(
            validator_auth.insecure_clone(),
            ":memory:",
            ChainConfig::local(ComputeBudgetConfig::new(1_000_000)),
            None,
            common::MockActionsCallbackExecutor::default(),
        )
        .unwrap(),
    );

    let counter_auth = Keypair::new();
    let (pubkey, mut account) = init_and_delegate_account_on_chain(
        &counter_auth,
        DELEGATED_ACCOUNT_SIZE,
        None,
    )
    .await;

    account.data = common::generate_random_bytes(bytes);
    account.owner = program_flexi_counter::id();

    let account = CommittedAccount {
        pubkey,
        account,
        remote_slot: Default::default(),
    };
    let base_intent = match commit_type {
        CommitIntentKind::Commit => {
            MagicBaseIntent::Commit(CommitType::Standalone(vec![account]))
        }
        CommitIntentKind::CommitFinalize => {
            MagicBaseIntent::CommitFinalize(CommitType::Standalone(vec![
                account,
            ]))
        }
        CommitIntentKind::CommitAndUndelegate => {
            MagicBaseIntent::CommitAndUndelegate(CommitAndUndelegate {
                commit_action: CommitType::Standalone(vec![account]),
                undelegate_action: UndelegateType::Standalone,
            })
        }
        CommitIntentKind::CommitFinalizeAndUndelegate => {
            MagicBaseIntent::CommitFinalizeAndUndelegate(CommitAndUndelegate {
                commit_action: CommitType::Standalone(vec![account]),
                undelegate_action: UndelegateType::Standalone,
            })
        }
    };

    let intent = ScheduledIntentBundle {
        id: 0,
        slot: 10,
        blockhash: Hash::new_unique(),
        sent_transaction: Transaction::default(),
        payer: counter_auth.pubkey(),
        intent_bundle: base_intent.into(),
    };

    ix_commit_local(
        processor,
        vec![intent],
        expect_strategies(&[(expected_strategy, 1)]),
        program_flexi_counter::ID,
    )
    .await;
}

// -----------------
// Multiple Account Commits
// -----------------

#[tokio::test]
async fn test_ix_commit_two_accounts_1kb_2kb() {
    init_logger!();
    commit_multiple_accounts(
        &[1024, 2048],
        1,
        CommitIntentKind::Commit,
        expect_strategies(&[(CommitStrategy::DiffArgs, 2)]),
    )
    .await;
}

#[tokio::test]
async fn test_ix_commit_two_accounts_512kb() {
    init_logger!();
    commit_multiple_accounts(
        &[512, 512],
        1,
        CommitIntentKind::Commit,
        expect_strategies(&[(CommitStrategy::DiffArgs, 2)]),
    )
    .await;
}

#[tokio::test]
async fn test_ix_commit_three_accounts_512kb() {
    init_logger!();
    commit_multiple_accounts(
        &[512, 512, 512],
        1,
        CommitIntentKind::Commit,
        expect_strategies(&[(CommitStrategy::DiffArgs, 3)]),
    )
    .await;
}

#[tokio::test]
async fn test_ix_commit_six_accounts_512kb() {
    init_logger!();
    commit_multiple_accounts(
        &[512, 512, 512, 512, 512, 512],
        1,
        CommitIntentKind::Commit,
        expect_strategies(&[(CommitStrategy::DiffArgs, 6)]),
    )
    .await;
}

#[tokio::test]
async fn test_ix_commit_four_accounts_1kb_2kb_5kb_10kb_single_bundle() {
    init_logger!();
    commit_multiple_accounts(
        &[1024, 2 * 1024, 5 * 1024, 10 * 1024],
        1,
        CommitIntentKind::Commit,
        expect_strategies(&[(CommitStrategy::DiffArgs, 4)]),
    )
    .await;
}

#[tokio::test]
async fn test_commit_20_accounts_1kb_bundle_size_2() {
    commit_20_accounts_1kb(
        2,
        expect_strategies(&[(CommitStrategy::DiffArgs, 20)]),
        CommitIntentKind::Commit,
    )
    .await;
}

#[tokio::test]
async fn test_commit_5_accounts_1kb_bundle_size_3() {
    commit_5_accounts_1kb(
        3,
        expect_strategies(&[(CommitStrategy::DiffArgs, 5)]),
        CommitIntentKind::Commit,
    )
    .await;
}

#[tokio::test]
async fn test_commit_5_accounts_1kb_bundle_size_3_undelegate_all() {
    commit_5_accounts_1kb(
        3,
        expect_strategies(&[
            // Intent fits in 1 TX only with ALT, see IntentExecutorImpl::try_unite_tasks
            (CommitStrategy::DiffArgs, 5),
        ]),
        CommitIntentKind::CommitAndUndelegate,
    )
    .await;
}

#[tokio::test]
async fn test_commit_5_accounts_1kb_bundle_size_4() {
    commit_5_accounts_1kb(
        4,
        expect_strategies(&[
            (CommitStrategy::DiffArgs, 1),
            (CommitStrategy::DiffBufferWithLookupTable, 4),
        ]),
        CommitIntentKind::Commit,
    )
    .await;
}

#[tokio::test]
async fn test_commit_5_accounts_1kb_bundle_size_4_undelegate_all() {
    commit_5_accounts_1kb(
        4,
        expect_strategies(&[
            (CommitStrategy::DiffArgs, 1),
            (CommitStrategy::DiffBufferWithLookupTable, 4),
        ]),
        CommitIntentKind::CommitAndUndelegate,
    )
    .await;
}

#[tokio::test]
async fn test_commit_5_accounts_1kb_bundle_size_5_undelegate_all() {
    commit_5_accounts_1kb(
        5,
        expect_strategies(&[(CommitStrategy::DiffBufferWithLookupTable, 5)]),
        CommitIntentKind::CommitAndUndelegate,
    )
    .await;
}

#[tokio::test]
async fn test_commit_20_accounts_1kb_bundle_size_3() {
    commit_20_accounts_1kb(
        3,
        expect_strategies(&[(CommitStrategy::DiffArgs, 20)]),
        CommitIntentKind::Commit,
    )
    .await;
}

#[tokio::test]
async fn test_commit_20_accounts_1kb_bundle_size_4() {
    commit_20_accounts_1kb(
        4,
        expect_strategies(&[(CommitStrategy::DiffBufferWithLookupTable, 20)]),
        CommitIntentKind::Commit,
    )
    .await;
}

#[tokio::test]
async fn test_commit_20_accounts_1kb_bundle_size_6() {
    commit_20_accounts_1kb(
        6,
        expect_strategies(&[
            (CommitStrategy::DiffBufferWithLookupTable, 18),
            // Two accounts don't make it into the bundles of size 6
            (CommitStrategy::DiffArgs, 2),
        ]),
        CommitIntentKind::Commit,
    )
    .await;
}

#[tokio::test]
async fn test_commit_20_accounts_1kb_bundle_size_5() {
    commit_20_accounts_1kb(
        5,
        expect_strategies(&[(CommitStrategy::DiffBufferWithLookupTable, 20)]),
        CommitIntentKind::Commit,
    )
    .await;
}

#[tokio::test]
async fn test_commit_8_accounts_1kb_bundle_size_8() {
    commit_8_accounts_1kb(
        8,
        expect_strategies(&[
            // Four accounts don't make it into the bundles of size 8, but
            // that bundle also needs lookup tables
            (CommitStrategy::DiffBufferWithLookupTable, 8),
        ]),
        CommitIntentKind::Commit,
    )
    .await;
}

#[tokio::test]
async fn test_commitfinalize_8_accounts_1kb_bundle_size_8() {
    commit_8_accounts_1kb(
        8,
        expect_strategies(&[
            // Four accounts don't make it into the bundles of size 8, but
            // that bundle also needs lookup tables
            (CommitStrategy::DiffBufferWithLookupTable, 8),
        ]),
        CommitIntentKind::CommitFinalize,
    )
    .await;
}

#[tokio::test]
async fn test_commit_20_accounts_1kb_bundle_size_8() {
    commit_20_accounts_1kb(
        8,
        expect_strategies(&[
            // Four accounts don't make it into the bundles of size 8, but
            // that bundle also needs lookup tables
            (CommitStrategy::DiffBufferWithLookupTable, 20),
        ]),
        CommitIntentKind::Commit,
    )
    .await;
}

#[tokio::test]
async fn test_commitfinalize_and_undelefate_20_accounts_1kb_bundle_size_11() {
    commit_20_accounts_1kb(
        11,
        expect_strategies(&[
            // Four accounts don't make it into the bundles of size 8, but
            // that bundle also needs lookup tables
            (CommitStrategy::DiffBufferWithLookupTable, 20),
        ]),
        CommitIntentKind::CommitFinalizeAndUndelegate,
    )
    .await;
}

#[tokio::test]
async fn test_commitfinalize_20_accounts_1kb_bundle_size_11() {
    commit_20_accounts_1kb(
        11,
        expect_strategies(&[
            // Four accounts don't make it into the bundles of size 8, but
            // that bundle also needs lookup tables
            (CommitStrategy::DiffBufferWithLookupTable, 20),
        ]),
        CommitIntentKind::CommitFinalize,
    )
    .await;
}

#[tokio::test]
async fn test_ix_execute_intent_bundle_commit_and_cau_simultaneously_union_of_accounts(
) {
    execute_intent_bundle(
        &[1024, 2048],
        &[],
        &[1024, 2048],
        expect_strategies(&[(CommitStrategy::DiffBufferWithLookupTable, 4)]),
    )
    .await;
}

#[tokio::test]
async fn test_ix_execute_intent_bundle_commit_three_accounts_cau_one_account() {
    execute_intent_bundle(
        &[512, 512, 512],
        &[],
        &[512],
        expect_strategies(&[(CommitStrategy::DiffBufferWithLookupTable, 4)]),
    )
    .await;
}

#[tokio::test]
async fn test_ix_execute_intent_bundle_mixed_fits_in_args() {
    execute_intent_bundle(
        &[10, 20, 10],
        &[],
        &[20],
        expect_strategies(&[(CommitStrategy::StateArgs, 4)]),
    )
    .await;
}

#[tokio::test]
async fn test_ix_execute_intent_bundle_commit_finalize_only() {
    execute_intent_bundle(
        &[],
        &[10, 20],
        &[],
        expect_strategies(&[(CommitStrategy::StateArgs, 2)]),
    )
    .await;
}

#[tokio::test]
async fn test_ix_execute_intent_bundle_commit_and_commit_finalize_mixed() {
    execute_intent_bundle(
        &[1024, 2048],
        &[1024, 2048],
        &[],
        expect_strategies(&[(CommitStrategy::DiffArgs, 4)]),
    )
    .await;
}

async fn commit_5_accounts_1kb(
    bundle_size: usize,
    expected_strategies: ExpectedStrategies,
    commit_type: CommitIntentKind,
) {
    init_logger!();
    let accs = (0..5).map(|_| 1024).collect::<Vec<_>>();
    commit_multiple_accounts(
        &accs,
        bundle_size,
        commit_type,
        expected_strategies,
    )
    .await;
}

async fn commit_8_accounts_1kb(
    bundle_size: usize,
    expected_strategies: ExpectedStrategies,
    commit_type: CommitIntentKind,
) {
    init_logger!();
    let accs = (0..8).map(|_| 1024).collect::<Vec<_>>();
    commit_multiple_accounts(
        &accs,
        bundle_size,
        commit_type,
        expected_strategies,
    )
    .await;
}

async fn commit_20_accounts_1kb(
    bundle_size: usize,
    expected_strategies: ExpectedStrategies,
    commit_type: CommitIntentKind,
) {
    init_logger!();
    let accs = (0..20).map(|_| 1024).collect::<Vec<_>>();
    commit_multiple_accounts(
        &accs,
        bundle_size,
        commit_type,
        expected_strategies,
    )
    .await;
}

async fn create_and_delegate_accounts(
    bytess: &[usize],
) -> Vec<CommittedAccount> {
    let mut join_set = JoinSet::new();
    for bytes in bytess {
        let bytes = *bytes;
        join_set.spawn(async move {
            let counter_auth = Keypair::new();
            let (pda, mut pda_acc) = init_and_delegate_account_on_chain(
                &counter_auth,
                bytes as u64,
                None,
            )
            .await;

            pda_acc.owner = program_flexi_counter::id();
            pda_acc.data = vec![0u8; bytes];
            CommittedAccount {
                pubkey: pda,
                account: pda_acc,
                remote_slot: Default::default(),
            }
        });
    }

    // Wait for all tasks to complete
    join_set.join_all().await
}

async fn create_bundles(
    bundle_size: usize,
    bytess: &[usize],
) -> Vec<Vec<CommittedAccount>> {
    let committed = create_and_delegate_accounts(bytess).await;
    committed
        .chunks(bundle_size)
        .map(|chunk| chunk.to_vec())
        .collect()
}

async fn commit_multiple_accounts(
    bytess: &[usize],
    bundle_size: usize,
    commit_type: CommitIntentKind,
    expected_strategies: ExpectedStrategies,
) {
    init_logger!();

    let validator_auth = ensure_validator_authority();
    fund_validator_auth_and_ensure_validator_fees_vault(&validator_auth).await;

    let processor = Arc::new(
        CommittorProcessor::try_new(
            validator_auth.insecure_clone(),
            ":memory:",
            ChainConfig::local(ComputeBudgetConfig::new(1_000_000)),
            None,
            common::MockActionsCallbackExecutor::default(),
        )
        .unwrap(),
    );

    // Create bundles of committed accounts
    let bundles_of_committees = create_bundles(bundle_size, bytess).await;
    // Create intent for each bundle
    let intents = bundles_of_committees
        .into_iter()
        .map(|committees| match commit_type {
            CommitIntentKind::Commit => {
                MagicBaseIntent::Commit(CommitType::Standalone(committees))
            }
            CommitIntentKind::CommitAndUndelegate => {
                MagicBaseIntent::CommitAndUndelegate(CommitAndUndelegate {
                    commit_action: CommitType::Standalone(committees),
                    undelegate_action: UndelegateType::Standalone,
                })
            }
            CommitIntentKind::CommitFinalize => {
                MagicBaseIntent::CommitFinalize(CommitType::Standalone(
                    committees,
                ))
            }
            CommitIntentKind::CommitFinalizeAndUndelegate => {
                MagicBaseIntent::CommitFinalizeAndUndelegate(
                    CommitAndUndelegate {
                        commit_action: CommitType::Standalone(committees),
                        undelegate_action: UndelegateType::Standalone,
                    },
                )
            }
        })
        .enumerate()
        .map(|(id, base_intent)| ScheduledIntentBundle {
            id: id as u64,
            slot: 0,
            blockhash: Hash::new_unique(),
            sent_transaction: Transaction::default(),
            payer: Pubkey::new_unique(),
            intent_bundle: base_intent.into(),
        })
        .collect::<Vec<_>>();

    ix_commit_local(
        processor,
        intents,
        expected_strategies,
        program_flexi_counter::ID,
    )
    .await;
}

async fn execute_intent_bundle(
    bytess_to_commit: &[usize],
    bytess_to_commit_finalize: &[usize],
    bytes_to_undelegate: &[usize],
    expected_strategies: ExpectedStrategies,
) {
    init_logger!();

    let validator_auth = ensure_validator_authority();
    fund_validator_auth_and_ensure_validator_fees_vault(&validator_auth).await;

    let processor = Arc::new(
        CommittorProcessor::try_new(
            validator_auth.insecure_clone(),
            ":memory:",
            ChainConfig::local(ComputeBudgetConfig::new(1_000_000)),
            None,
            common::MockActionsCallbackExecutor::default(),
        )
        .unwrap(),
    );

    // Create bundles of committed accounts
    let to_commit = create_and_delegate_accounts(bytess_to_commit);
    let to_commit_finalize =
        create_and_delegate_accounts(bytess_to_commit_finalize);
    let to_undelegate = create_and_delegate_accounts(bytes_to_undelegate);
    let (committees, commit_finalize_committees, undelegetees) =
        tokio::join!(to_commit, to_commit_finalize, to_undelegate);

    let mut intent_bundle = MagicIntentBundle::default();
    if !committees.is_empty() {
        intent_bundle.commit = Some(CommitType::Standalone(committees));
    }
    if !commit_finalize_committees.is_empty() {
        intent_bundle.commit_finalize =
            Some(CommitType::Standalone(commit_finalize_committees));
    }
    if !undelegetees.is_empty() {
        intent_bundle.commit_and_undelegate = Some(CommitAndUndelegate {
            commit_action: CommitType::Standalone(undelegetees),
            undelegate_action: UndelegateType::Standalone,
        });
    }

    // Create intent for each bundle
    let intent_bundle = ScheduledIntentBundle {
        id: 0,
        slot: 0,
        blockhash: Hash::new_unique(),
        sent_transaction: Transaction::default(),
        payer: Pubkey::new_unique(),
        intent_bundle,
    };
    ix_commit_local(
        processor,
        vec![intent_bundle],
        expected_strategies,
        program_flexi_counter::id(),
    )
    .await;
}

// TODO(thlorenz/snawaz): once delegation program supports larger commits add the following
//                 tests
//
// ## Scenario 1
//
// All realloc instructions still fit into the same transaction as the init instruction
// of each account

// ## Scenario 2
//
// Max size that is allowed on solana (10MB)
// https://solana.com/docs/core/accounts
// 9,996,760 bytes 9.53MB requiring 69.57 SOL to be rent exempt

// This requires a chunk tracking account of 1.30KB which can be fully allocated
// as part of the init instruction. Since no larger buffers are possible this
// chunk account size suffices and we don't have to worry about reallocs
// of that tracking account

// This test pushes the validator to the max, sending >10K transactions in
// order to allocate enough space and write the chunks.
// It shows that committing buffers in that size range is not practically
// feasible, but still we ensure here that it is handled.

// -----------------
// Test Executor
// -----------------

/// For each committed account across `intent_bundles`, fetches its current
/// on-chain balance and its `DelegationRecord.lamports` ledger value, keyed
/// by pubkey. Missing/undeserializable records are omitted (the caller falls
/// back to treating the account as having no prior growth).
async fn collect_pre_execution_lamports(
    rpc_client: &RpcClient,
    intent_bundles: &[ScheduledIntentBundle],
) -> HashMap<Pubkey, (u64, u64)> {
    let mut pre_state = HashMap::new();
    for base_intent in intent_bundles {
        let pubkeys: Vec<Pubkey> = [
            base_intent.get_commit_intent_accounts(),
            base_intent.get_commit_finalize_intent_accounts(),
            base_intent.get_undelegate_intent_accounts(),
            base_intent.get_commit_finalize_and_undelegate_intent_accounts(),
        ]
        .into_iter()
        .flatten()
        .flat_map(|accounts| accounts.iter())
        .map(|account| account.pubkey)
        .collect();

        for pubkey in pubkeys {
            if pre_state.contains_key(&pubkey) {
                continue;
            }
            let Ok(balance_before) = rpc_client.get_balance(&pubkey).await
            else {
                continue;
            };
            let delegation_record_pda =
                dlp_api::pda::delegation_record_pda_from_delegated_account(
                    &pubkey,
                );
            let delegation_record_lamports = rpc_client
                .get_account(&delegation_record_pda)
                .await
                .ok()
                .and_then(|acc| {
                    dlp_api::state::DelegationRecord::try_from_bytes_with_discriminator(
                        &acc.data,
                    )
                    .ok()
                    .map(|record| record.lamports)
                })
                .unwrap_or(balance_before);
            pre_state
                .insert(pubkey, (balance_before, delegation_record_lamports));
        }
    }
    pre_state
}

/// Computes what `pubkey`'s real final balance should be after committing
/// `committed_lamports` lamports at `new_data_len` bytes, mirroring dlp's own
/// settlement math (see `finalize.rs`/`commit_finalize_internal.rs`): the
/// committed lamports value is a *delta* against `DelegationRecord.lamports`,
/// applied on top of whatever the account already holds -- which may be more
/// than its pre-commit balance if PreallocateBuffer grew (and funded) it.
fn expected_settled_lamports(
    pre_state: &HashMap<Pubkey, (u64, u64)>,
    pubkey: Pubkey,
    committed_lamports: u64,
    new_data_len: usize,
) -> u64 {
    let (balance_before, delegation_record_lamports_before) = pre_state
        .get(&pubkey)
        .copied()
        .unwrap_or((committed_lamports, committed_lamports));
    let rent_exempt_minimum =
        solana_sdk::rent::Rent::default().minimum_balance(new_data_len);
    let base = balance_before.max(rent_exempt_minimum);
    (base as i128 + committed_lamports as i128
        - delegation_record_lamports_before as i128) as u64
}

async fn ix_commit_local(
    processor: Arc<CommittorProcessor>,
    intent_bundles: Vec<ScheduledIntentBundle>,
    expected_strategies: ExpectedStrategies,
    program_id: Pubkey,
) {
    let rpc_client = RpcClient::new_with_commitment(
        "http://localhost:7799".to_string(),
        CommitmentConfig::confirmed(),
    );

    // Fetch pre intent state
    let pre_state = collect_pre_execution_lamports(&rpc_client, &intent_bundles)
        .await;

    let execution_outputs = processor
        .execute_intent_bundles(intent_bundles.clone())
        .await
        .unwrap()
        .into_iter()
        .collect::<Vec<_>>();

    // Assert that all completed
    assert_eq!(execution_outputs.len(), intent_bundles.len());

    let mut strategies = ExpectedStrategies::new();
    for (execution_result, base_intent) in execution_outputs
        .into_iter()
        .zip(intent_bundles.into_iter())
    {
        if !execution_result.patched_errors.is_empty() {
            panic!("Failed to execute without patching: {:?}", execution_result.patched_errors);
        }
        let output = match execution_result.inner {
            Ok(output) => output,
            Err(err) => {
                match &*err {
                    IntentExecutorError::FailedToFinalizeError {
                        commit_signature,
                        finalize_signature,
                        ..
                    } => {
                        if let Some(commit_sig) = commit_signature {
                            print_tx_logs(&rpc_client, commit_sig).await;
                        }
                        if let Some(finalize_sig) = finalize_signature {
                            print_tx_logs(&rpc_client, finalize_sig).await;
                        }
                    }
                    IntentExecutorError::FailedToCommitError {
                        signature: Some(signature),
                        ..
                    } => {
                        print_tx_logs(&rpc_client, signature).await;
                    }
                    _ => {}
                }
                panic!("Intent execution failed: {err:?}");
            }
        };
        let (commit_signature, finalize_signature) = match output {
            ExecutionOutput::SingleStage(signature) => (signature, signature),
            ExecutionOutput::TwoStage {
                commit_signature,
                finalize_signature,
            } => (commit_signature, finalize_signature),
        };
        debug!("commit signature: {}", commit_signature);
        debug!("finalize signature: {}", finalize_signature);

        let committed_accounts = base_intent.get_commit_intent_accounts();
        let committed_finalize_accounts =
            base_intent.get_commit_finalize_intent_accounts();
        let undelegated_accounts = base_intent.get_undelegate_intent_accounts();
        let commit_finalized_and_undelegated_accounts =
            base_intent.get_commit_finalize_and_undelegate_intent_accounts();

        let has_undelegate = base_intent.has_undelegate_intent();

        let mut committed_accounts: HashMap<Pubkey, _> = [
            (false, committed_accounts),
            (true, undelegated_accounts),
            (false, committed_finalize_accounts),
            (true, commit_finalized_and_undelegated_accounts),
        ]
        .into_iter()
        .flat_map(|(allow_undelegation, accounts)| {
            accounts.into_iter().flatten().map(move |account| {
                (account.pubkey, (allow_undelegation, account))
            })
        })
        .collect();

        let statuses = processor.get_commit_statuses(base_intent.id).unwrap();
        debug!(
            "{}",
            statuses
                .iter()
                .map(|x| x.to_string())
                .collect::<Vec<_>>()
                .join("\n")
        );

        assert_eq!(statuses.len(), committed_accounts.len());

        for commit_status in statuses {
            let (is_undelegate, account) = committed_accounts
                .remove(&commit_status.pubkey)
                .expect("Account should be persisted");
            println!("account: {}", account.pubkey);

            // When we finalize it is possible to also undelegate the account
            let expected_owner = if is_undelegate {
                program_id
            } else {
                dlp_api::id()
            };

            let lamports = expected_settled_lamports(
                &pre_state,
                account.pubkey,
                account.account.lamports,
                account.account.data.len(),
            );
            get_account!(
                rpc_client,
                account.pubkey,
                "delegated state",
                |acc: &Account, remaining_tries: u8| {
                    validate_account(
                        acc,
                        remaining_tries,
                        &account.account.data,
                        lamports,
                        expected_owner,
                        account.pubkey,
                        has_undelegate,
                    )
                }
            );

            // Track the strategy used
            let strategy = commit_status.commit_strategy;
            let strategy_count = strategies.entry(strategy).or_insert(0);
            *strategy_count += 1;
        }
    }

    // Compare the strategies used with the expected ones
    debug!("Strategies used: {:?}", strategies);
    assert_eq!(
        strategies, expected_strategies,
        "Strategies used do not match expected ones"
    );
}

fn validate_account(
    acc: &Account,
    remaining_tries: u8,
    expected_data: &[u8],
    expected_lamports: u64,
    expected_owner: Pubkey,
    account_pubkey: Pubkey,
    is_undelegate: bool,
) -> bool {
    let matches_data = acc.data() == expected_data;
    let matched_balance = acc.lamports() == expected_lamports;
    let matches_data = matches_data && matched_balance;
    let matches_undelegation = acc.owner().eq(&expected_owner);
    let matches_all = matches_data && matches_undelegation;

    if !matches_all && remaining_tries.is_multiple_of(4) {
        if !matches_data {
            trace!(
                "Account ({}) data {} != {} || {} != {}",
                account_pubkey,
                acc.data().len(),
                expected_data.len(),
                acc.lamports(),
                expected_lamports
            );
        }
        if !matches_undelegation {
            trace!(
                "Account ({}) is {} but should be. Owner {} != {}",
                account_pubkey,
                if is_undelegate {
                    "not undelegated"
                } else {
                    "undelegated"
                },
                acc.owner(),
                expected_owner,
            );
        }
    }
    matches_all
}
