use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant},
};

use borsh::to_vec;
use magicblock_committor_service::{
    config::ChainConfig,
    intent_executor::ExecutionOutput,
    persist::CommitStrategy,
    service_ext::{BaseIntentCommittorExt, CommittorServiceExt},
    BaseIntentCommittor, CommittorService, ComputeBudgetConfig,
};
use magicblock_program::magic_scheduled_base_intent::{
    CommitAndUndelegate, CommitType, CommittedAccount, MagicBaseIntent,
    MagicIntentBundle, ScheduledIntentBundle, UndelegateType,
};
use magicblock_rpc_client::MagicblockRpcClient;
use program_flexi_counter::state::FlexiCounter;
use solana_account::{Account, ReadableAccount};
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig, hash::Hash, signature::Keypair,
    signer::Signer, transaction::Transaction,
};
use test_kit::init_logger;
use tokio::task::JoinSet;
use tracing::*;
use utils::transactions::tx_logs_contain;

use self::utils::transactions::init_and_delegate_order_book_on_chain;
use crate::utils::{
    ensure_validator_authority,
    transactions::{
        fund_validator_auth_and_ensure_validator_fees_vault,
        init_and_delegate_account_on_chain,
    },
};

mod utils;

// -----------------
// Utilities and Setup
// -----------------
type ExpectedStrategies = HashMap<CommitStrategy, u8>;

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
    commit_single_account(100, CommitStrategy::StateArgs, false).await;
}

#[tokio::test]
async fn test_ix_commit_single_account_100_bytes_and_undelegate() {
    commit_single_account(100, CommitStrategy::StateArgs, true).await;
}

#[tokio::test]
async fn test_ix_commit_single_account_256_bytes() {
    commit_single_account(256, CommitStrategy::StateArgs, false).await;
}

#[tokio::test]
async fn test_ix_commit_single_account_257_bytes() {
    commit_single_account(257, CommitStrategy::DiffArgs, false).await;
}

#[tokio::test]
async fn test_ix_commit_single_account_256_bytes_and_undelegate() {
    commit_single_account(256, CommitStrategy::StateArgs, true).await;
}

#[tokio::test]
async fn test_ix_commit_single_account_257_bytes_and_undelegate() {
    commit_single_account(257, CommitStrategy::DiffArgs, true).await;
}

#[tokio::test]
async fn test_ix_commit_single_account_800_bytes() {
    commit_single_account(800, CommitStrategy::DiffArgs, false).await;
}

#[tokio::test]
async fn test_ix_commit_single_account_800_bytes_and_undelegate() {
    commit_single_account(800, CommitStrategy::DiffArgs, true).await;
}

#[tokio::test]
async fn test_ix_commit_single_account_one_kb() {
    commit_single_account(1024, CommitStrategy::DiffArgs, false).await;
}

#[tokio::test]
async fn test_ix_commit_single_account_ten_kb() {
    commit_single_account(10 * 1024, CommitStrategy::DiffArgs, false).await;
}

#[tokio::test]
async fn test_ix_commit_order_book_change_100_bytes() {
    commit_book_order_account(100, CommitStrategy::DiffArgs, false).await;
}

#[tokio::test]
async fn test_ix_commit_order_book_change_671_bytes() {
    commit_book_order_account(671, CommitStrategy::DiffArgs, false).await;
}

#[tokio::test]
async fn test_ix_commit_order_book_change_673_bytes() {
    // We cannot use 672 as changed_len because that both 671 and 672 produce encoded tx
    // of size 1644 (which is the max limit), but while the size of raw bytes for 671 is within
    // 1232 limit, the size for 672 exceeds by 1 (1233). That is why we used
    // 673 as changed_len where CommitStrategy goes from Args to FromBuffer.
    commit_book_order_account(673, CommitStrategy::DiffBuffer, false).await;
}

#[tokio::test]
async fn test_ix_commit_order_book_change_10k_bytes() {
    commit_book_order_account(10 * 1024, CommitStrategy::DiffBuffer, false)
        .await;
}

async fn commit_single_account(
    bytes: usize,
    expected_strategy: CommitStrategy,
    undelegate: bool,
) {
    init_logger!();

    let validator_auth = ensure_validator_authority();
    fund_validator_auth_and_ensure_validator_fees_vault(&validator_auth).await;

    // Run each test with and without finalizing
    let service = CommittorService::try_start(
        validator_auth.insecure_clone(),
        ":memory:",
        ChainConfig::local(ComputeBudgetConfig::new(1_000_000)),
    )
    .unwrap();
    let service = CommittorServiceExt::new(Arc::new(service));

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
    let base_intent = if undelegate {
        MagicBaseIntent::CommitAndUndelegate(CommitAndUndelegate {
            commit_action: CommitType::Standalone(vec![account]),
            undelegate_action: UndelegateType::Standalone,
        })
    } else {
        MagicBaseIntent::Commit(CommitType::Standalone(vec![account]))
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
        service,
        vec![intent],
        expect_strategies(&[(expected_strategy, 1)]),
    )
    .await;
}

async fn commit_book_order_account(
    changed_len: usize,
    expected_strategy: CommitStrategy,
    undelegate: bool,
) {
    init_logger!();

    let validator_auth = ensure_validator_authority();
    fund_validator_auth_and_ensure_validator_fees_vault(&validator_auth).await;

    // Run each test with and without finalizing
    let service = CommittorService::try_start(
        validator_auth.insecure_clone(),
        ":memory:",
        ChainConfig::local(ComputeBudgetConfig::new(1_000_000)),
    )
    .unwrap();
    let service = CommittorServiceExt::new(Arc::new(service));

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
    let base_intent = if undelegate {
        MagicBaseIntent::CommitAndUndelegate(CommitAndUndelegate {
            commit_action: CommitType::Standalone(vec![account]),
            undelegate_action: UndelegateType::Standalone,
        })
    } else {
        MagicBaseIntent::Commit(CommitType::Standalone(vec![account]))
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
        service,
        vec![intent],
        expect_strategies(&[(expected_strategy, 1)]),
    )
    .await;
}

// TODO(thlorenz/snawaz): once delegation program supports larger commits
// add 1MB and 10MB tests

// -----------------
// Multiple Account Commits
// -----------------

#[tokio::test]
async fn test_ix_commit_two_accounts_1kb_2kb() {
    init_logger!();
    commit_multiple_accounts(
        &[1024, 2048],
        1,
        false,
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
        false,
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
        false,
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
        false,
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
        false,
        expect_strategies(&[(CommitStrategy::DiffArgs, 4)]),
    )
    .await;
}

#[tokio::test]
async fn test_commit_20_accounts_1kb_bundle_size_2() {
    commit_20_accounts_1kb(
        2,
        expect_strategies(&[(CommitStrategy::DiffArgs, 20)]),
    )
    .await;
}

#[tokio::test]
async fn test_commit_5_accounts_1kb_bundle_size_3() {
    commit_5_accounts_1kb(
        3,
        expect_strategies(&[(CommitStrategy::DiffArgs, 5)]),
        false,
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
        true,
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
        false,
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
        true,
    )
    .await;
}

#[tokio::test]
async fn test_commit_5_accounts_1kb_bundle_size_5_undelegate_all() {
    commit_5_accounts_1kb(
        5,
        expect_strategies(&[(CommitStrategy::DiffBufferWithLookupTable, 5)]),
        true,
    )
    .await;
}

#[tokio::test]
async fn test_commit_20_accounts_1kb_bundle_size_3() {
    commit_20_accounts_1kb(
        3,
        expect_strategies(&[(CommitStrategy::DiffArgs, 20)]),
    )
    .await;
}

#[tokio::test]
async fn test_commit_20_accounts_1kb_bundle_size_4() {
    commit_20_accounts_1kb(
        4,
        expect_strategies(&[(CommitStrategy::DiffBufferWithLookupTable, 20)]),
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
    )
    .await;
}

#[tokio::test]
async fn test_commit_20_accounts_1kb_bundle_size_20() {
    commit_20_accounts_1kb(
        20,
        expect_strategies(&[(CommitStrategy::DiffBufferWithLookupTable, 20)]),
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
    )
    .await;
}

#[tokio::test]
async fn test_ix_execute_intent_bundle_commit_and_cau_simultaneously_union_of_accounts(
) {
    execute_intent_bundle(
        &[1024, 2048],
        &[1024, 2048],
        expect_strategies(&[(CommitStrategy::DiffBufferWithLookupTable, 4)]),
    )
    .await;
}

#[tokio::test]
async fn test_ix_execute_intent_bundle_commit_three_accounts_cau_one_account() {
    execute_intent_bundle(
        &[512, 512, 512],
        &[512],
        expect_strategies(&[(CommitStrategy::DiffBufferWithLookupTable, 4)]),
    )
    .await;
}

#[tokio::test]
async fn test_ix_execute_intent_bundle_mixed_fits_in_args() {
    execute_intent_bundle(
        &[10, 20, 10],
        &[20],
        expect_strategies(&[(CommitStrategy::StateArgs, 4)]),
    )
    .await;
}

async fn commit_5_accounts_1kb(
    bundle_size: usize,
    expected_strategies: ExpectedStrategies,
    undelegate_all: bool,
) {
    init_logger!();
    let accs = (0..5).map(|_| 1024).collect::<Vec<_>>();
    commit_multiple_accounts(
        &accs,
        bundle_size,
        undelegate_all,
        expected_strategies,
    )
    .await;
}

async fn commit_8_accounts_1kb(
    bundle_size: usize,
    expected_strategies: ExpectedStrategies,
) {
    init_logger!();
    let accs = (0..8).map(|_| 1024).collect::<Vec<_>>();
    commit_multiple_accounts(&accs, bundle_size, false, expected_strategies)
        .await;
}

async fn commit_20_accounts_1kb(
    bundle_size: usize,
    expected_strategies: ExpectedStrategies,
) {
    init_logger!();
    let accs = (0..20).map(|_| 1024).collect::<Vec<_>>();
    commit_multiple_accounts(&accs, bundle_size, false, expected_strategies)
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
    undelegate_all: bool,
    expected_strategies: ExpectedStrategies,
) {
    init_logger!();

    let validator_auth = ensure_validator_authority();
    fund_validator_auth_and_ensure_validator_fees_vault(&validator_auth).await;

    let service = CommittorService::try_start(
        validator_auth.insecure_clone(),
        ":memory:",
        ChainConfig::local(ComputeBudgetConfig::new(1_000_000)),
    )
    .unwrap();
    let service = CommittorServiceExt::new(Arc::new(service));

    // Create bundles of committed accounts
    let bundles_of_committees = create_bundles(bundle_size, bytess).await;
    // Create intent for each bundle
    let intents = bundles_of_committees
        .into_iter()
        .map(|committees| {
            if undelegate_all {
                MagicBaseIntent::CommitAndUndelegate(CommitAndUndelegate {
                    commit_action: CommitType::Standalone(committees),
                    undelegate_action: UndelegateType::Standalone,
                })
            } else {
                MagicBaseIntent::Commit(CommitType::Standalone(committees))
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

    ix_commit_local(service, intents, expected_strategies).await;
}

async fn execute_intent_bundle(
    bytess_to_commit: &[usize],
    bytes_to_undelegate: &[usize],
    expected_strategies: ExpectedStrategies,
) {
    init_logger!();

    let validator_auth = ensure_validator_authority();
    fund_validator_auth_and_ensure_validator_fees_vault(&validator_auth).await;

    let service = CommittorService::try_start(
        validator_auth.insecure_clone(),
        ":memory:",
        ChainConfig::local(ComputeBudgetConfig::new(1_000_000)),
    )
    .unwrap();
    let service = CommittorServiceExt::new(Arc::new(service));

    // Create bundles of committed accounts
    let to_commit = create_and_delegate_accounts(bytess_to_commit);
    let to_undelegate = create_and_delegate_accounts(bytes_to_undelegate);
    let (committees, undelegetees) = tokio::join!(to_commit, to_undelegate);

    let mut intent_bundle = MagicIntentBundle::default();
    if !committees.is_empty() {
        intent_bundle.commit = Some(CommitType::Standalone(committees));
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
    ix_commit_local(service, vec![intent_bundle], expected_strategies).await;
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
async fn ix_commit_local(
    service: CommittorServiceExt<CommittorService>,
    intent_bundles: Vec<ScheduledIntentBundle>,
    expected_strategies: ExpectedStrategies,
) {
    let execution_outputs = service
        .schedule_intent_bundles_waiting(intent_bundles.clone())
        .await
        .unwrap()
        .into_iter()
        .collect::<Vec<_>>();

    // Assert that all completed
    assert_eq!(execution_outputs.len(), intent_bundles.len());
    service.release_common_pubkeys().await.unwrap();

    let rpc_client = RpcClient::new("http://localhost:7799".to_string());
    let mut strategies = ExpectedStrategies::new();
    for (execution_result, base_intent) in execution_outputs
        .into_iter()
        .zip(intent_bundles.into_iter())
    {
        let output = execution_result.inner.unwrap();
        let (commit_signature, finalize_signature) = match output {
            ExecutionOutput::SingleStage(signature) => (signature, signature),
            ExecutionOutput::TwoStage {
                commit_signature,
                finalize_signature,
            } => (commit_signature, finalize_signature),
        };

        assert_eq!(
            execution_result.patched_errors.len(),
            0,
            "No errors expected to be patched"
        );
        assert!(
            tx_logs_contain(&rpc_client, &commit_signature, "CommitState")
                .await
        );
        assert!(
            tx_logs_contain(&rpc_client, &finalize_signature, "Finalize").await
        );

        let has_undelegate = base_intent.has_undelegate_intent();
        if has_undelegate {
            // Undelegate is part of atomic Finalization Stage
            assert!(
                tx_logs_contain(&rpc_client, &finalize_signature, "Undelegate")
                    .await
            );
        }

        let committed_accounts = base_intent.get_commit_intent_accounts();
        let undelegated_accounts = base_intent.get_undelegate_intent_accounts();
        let mut committed_accounts: HashMap<Pubkey, _> =
            [(false, committed_accounts), (true, undelegated_accounts)]
                .into_iter()
                .flat_map(|(allow_undelegation, accounts)| {
                    accounts.into_iter().flatten().map(move |account| {
                        (account.pubkey, (allow_undelegation, account))
                    })
                })
                .collect();

        let statuses = service
            .get_commit_statuses(base_intent.id)
            .await
            .unwrap()
            .unwrap();
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

            // When we finalize it is possible to also undelegate the account
            let expected_owner = if is_undelegate {
                program_flexi_counter::id()
            } else {
                dlp::id()
            };

            let lamports = account.account.lamports;
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

    let expect_empty_lookup_tables = false;
    // changeset.accounts.len() == changeset.accounts_to_undelegate.len();
    if expect_empty_lookup_tables {
        let lookup_tables = service.get_lookup_tables().await.unwrap();
        assert!(lookup_tables.active.is_empty());

        if utils::TEST_TABLE_CLOSE {
            let mut closing_tables = lookup_tables.released;

            // Tables deactivate after ~2.5 mins (150secs), but most times
            // it takes a lot longer so we allow double the time
            const MAX_TIME_TO_CLOSE: Duration = Duration::from_secs(300);
            info!(
                "Waiting for lookup tables close for up to {} secs",
                MAX_TIME_TO_CLOSE.as_secs()
            );

            let start = Instant::now();
            let rpc_client = MagicblockRpcClient::from(rpc_client);
            loop {
                let accs = rpc_client
                    .get_multiple_accounts_with_commitment(
                        &closing_tables,
                        CommitmentConfig::confirmed(),
                        None,
                    )
                    .await
                    .unwrap();
                let closed_pubkeys = accs
                    .into_iter()
                    .zip(closing_tables.iter())
                    .filter_map(|(acc, pubkey)| {
                        if acc.is_none() {
                            Some(*pubkey)
                        } else {
                            None
                        }
                    })
                    .collect::<HashSet<_>>();
                closing_tables.retain(|pubkey| {
                    if closed_pubkeys.contains(pubkey) {
                        debug!("Table {} closed", pubkey);
                        false
                    } else {
                        true
                    }
                });
                if closing_tables.is_empty() {
                    break;
                }
                debug!(
                    "Still waiting for {} released table(s) to close",
                    closing_tables.len()
                );
                if Instant::now() - start > MAX_TIME_TO_CLOSE {
                    panic!(
                        "Timed out waiting for tables close after {} seconds. Still open: {}",
                        MAX_TIME_TO_CLOSE.as_secs(),
                        closing_tables
                            .iter()
                            .map(|x| x.to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                }
                utils::sleep_millis(10_000).await;
            }
        }
    }
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
    let matches_data =
        acc.data() == expected_data && acc.lamports() == expected_lamports;
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
