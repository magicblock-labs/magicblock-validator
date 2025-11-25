use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant},
};

use borsh::to_vec;
use compressed_delegation_client::CompressedDelegationRecord;
use light_client::indexer::{
    photon_indexer::PhotonIndexer, CompressedAccount, Indexer,
};
use log::*;
use magicblock_chainlink::testing::utils::{PHOTON_URL, RPC_URL};
use magicblock_committor_service::{
    config::ChainConfig,
    intent_executor::ExecutionOutput,
    persist::CommitStrategy,
    service_ext::{BaseIntentCommittorExt, CommittorServiceExt},
    types::{ScheduledBaseIntentWrapper, TriggerType},
    BaseIntentCommittor, CommittorService, ComputeBudgetConfig,
};
use magicblock_core::compression::derive_cda_from_pda;
use magicblock_program::magic_scheduled_base_intent::{
    CommitAndUndelegate, CommitType, CommittedAccount, MagicBaseIntent,
    ScheduledBaseIntent, UndelegateType,
};
use magicblock_rpc_client::MagicblockRpcClient;
use program_flexi_counter::state::FlexiCounter;
use solana_account::{Account, ReadableAccount};
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig, hash::Hash, rent::Rent,
    signature::Keypair, signer::Signer, transaction::Transaction,
};
use test_kit::init_logger;
use tokio::task::JoinSet;
use utils::transactions::tx_logs_contain;

use crate::utils::{
    ensure_validator_authority,
    transactions::{
        fund_validator_auth_and_ensure_validator_fees_vault,
        init_and_delegate_account_on_chain,
        init_and_delegate_compressed_account_on_chain,
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
    commit_single_account(100, CommitStrategy::Args, CommitAccountMode::Commit)
        .await;
}

#[tokio::test]
async fn test_ix_commit_single_account_100_bytes_and_undelegate() {
    commit_single_account(
        100,
        CommitStrategy::Args,
        CommitAccountMode::CommitAndUndelegate,
    )
    .await;
}

#[tokio::test]
async fn test_ix_commit_single_account_800_bytes() {
    commit_single_account(
        800,
        CommitStrategy::FromBuffer,
        CommitAccountMode::Commit,
    )
    .await;
}

#[tokio::test]
async fn test_ix_commit_single_account_800_bytes_and_undelegate() {
    commit_single_account(
        800,
        CommitStrategy::FromBuffer,
        CommitAccountMode::CommitAndUndelegate,
    )
    .await;
}

#[tokio::test]
async fn test_ix_commit_single_account_one_kb() {
    commit_single_account(
        1024,
        CommitStrategy::FromBuffer,
        CommitAccountMode::Commit,
    )
    .await;
}

#[tokio::test]
async fn test_ix_commit_single_account_ten_kb() {
    commit_single_account(
        10 * 1024,
        CommitStrategy::FromBuffer,
        CommitAccountMode::Commit,
    )
    .await;
}

enum CommitAccountMode {
    Commit,
    CompressedCommit,
    CommitAndUndelegate,
    CompressedCommitAndUndelegate,
}

async fn commit_single_account(
    bytes: usize,
    expected_strategy: CommitStrategy,
    mode: CommitAccountMode,
) {
    init_logger!();

    let validator_auth = ensure_validator_authority();
    fund_validator_auth_and_ensure_validator_fees_vault(&validator_auth).await;

    let photon_client =
        Arc::new(PhotonIndexer::new(PHOTON_URL.to_string(), None));

    // Run each test with and without finalizing
    let service = CommittorService::try_start(
        validator_auth.insecure_clone(),
        ":memory:",
        ChainConfig::local(ComputeBudgetConfig::new(1_000_000)),
        photon_client,
    )
    .unwrap();
    let service = CommittorServiceExt::new(Arc::new(service));

    let counter_auth = Keypair::new();
    let (pubkey, mut account) = match mode {
        CommitAccountMode::Commit | CommitAccountMode::CommitAndUndelegate => {
            init_and_delegate_account_on_chain(
                &counter_auth,
                bytes as u64,
                None,
            )
            .await
        }
        CommitAccountMode::CompressedCommit
        | CommitAccountMode::CompressedCommitAndUndelegate => {
            let (pubkey, _hash, account) =
                init_and_delegate_compressed_account_on_chain(&counter_auth)
                    .await;
            (pubkey, account)
        }
    };

    let counter = FlexiCounter {
        label: "Counter".to_string(),
        updates: 0,
        count: 101,
    };
    let mut data = to_vec(&counter).unwrap();
    data.resize(bytes, 0);
    account.data = data;
    account.owner = program_flexi_counter::id();

    let account = CommittedAccount { pubkey, account };
    let base_intent = match mode {
        CommitAccountMode::CommitAndUndelegate => {
            MagicBaseIntent::CommitAndUndelegate(CommitAndUndelegate {
                commit_action: CommitType::Standalone(vec![account]),
                undelegate_action: UndelegateType::Standalone,
            })
        }
        CommitAccountMode::Commit => {
            MagicBaseIntent::Commit(CommitType::Standalone(vec![account]))
        }
        CommitAccountMode::CompressedCommit => {
            MagicBaseIntent::CompressedCommit(CommitType::Standalone(vec![
                account,
            ]))
        }
        CommitAccountMode::CompressedCommitAndUndelegate => {
            MagicBaseIntent::CompressedCommitAndUndelegate(
                CommitAndUndelegate {
                    commit_action: CommitType::Standalone(vec![account]),
                    undelegate_action: UndelegateType::Standalone,
                },
            )
        }
    };

    let intent = ScheduledBaseIntentWrapper {
        trigger_type: TriggerType::OnChain,
        inner: ScheduledBaseIntent {
            id: 0,
            slot: 10,
            blockhash: Hash::new_unique(),
            action_sent_transaction: Transaction::default(),
            payer: counter_auth.pubkey(),
            base_intent,
        },
    };

    // We should always be able to Commit & Finalize 1 account either with Args or Buffers
    ix_commit_local(
        service,
        vec![intent],
        expect_strategies(&[(expected_strategy, 1)]),
    )
    .await;
}

// TODO(thlorenz): once delegation program supports larger commits
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
        CommitAccountMode::Commit,
        expect_strategies(&[(CommitStrategy::FromBuffer, 2)]),
    )
    .await;
}

#[tokio::test]
async fn test_ix_commit_two_accounts_512kb() {
    init_logger!();
    commit_multiple_accounts(
        &[512, 512],
        1,
        CommitAccountMode::Commit,
        expect_strategies(&[(CommitStrategy::Args, 2)]),
    )
    .await;
}

#[tokio::test]
async fn test_ix_commit_three_accounts_512kb() {
    init_logger!();
    commit_multiple_accounts(
        &[512, 512, 512],
        1,
        CommitAccountMode::Commit,
        expect_strategies(&[(CommitStrategy::Args, 3)]),
    )
    .await;
}

#[tokio::test]
async fn test_ix_commit_six_accounts_512kb() {
    init_logger!();
    commit_multiple_accounts(
        &[512, 512, 512, 512, 512, 512],
        1,
        CommitAccountMode::Commit,
        expect_strategies(&[(CommitStrategy::Args, 6)]),
    )
    .await;
}

#[tokio::test]
async fn test_ix_commit_four_accounts_1kb_2kb_5kb_10kb_single_bundle() {
    init_logger!();
    commit_multiple_accounts(
        &[1024, 2 * 1024, 5 * 1024, 10 * 1024],
        1,
        CommitAccountMode::Commit,
        expect_strategies(&[(CommitStrategy::FromBuffer, 4)]),
    )
    .await;
}

#[tokio::test]
async fn test_commit_20_accounts_1kb_bundle_size_2() {
    commit_20_accounts_1kb(
        2,
        expect_strategies(&[(CommitStrategy::FromBuffer, 20)]),
    )
    .await;
}

#[tokio::test]
async fn test_commit_5_accounts_1kb_bundle_size_3() {
    commit_5_accounts_1kb(
        3,
        expect_strategies(&[(CommitStrategy::FromBuffer, 5)]),
        CommitAccountMode::Commit,
    )
    .await;
}

#[tokio::test]
async fn test_commit_5_accounts_1kb_bundle_size_3_undelegate_all() {
    commit_5_accounts_1kb(
        3,
        expect_strategies(&[
            // Intent fits in 1 TX only with ALT, see IntentExecutorImpl::try_unite_tasks
            (CommitStrategy::FromBufferWithLookupTable, 3),
            (CommitStrategy::FromBuffer, 2),
        ]),
        CommitAccountMode::CommitAndUndelegate,
    )
    .await;
}

#[tokio::test]
async fn test_commit_5_accounts_1kb_bundle_size_4() {
    commit_5_accounts_1kb(
        4,
        expect_strategies(&[
            (CommitStrategy::FromBuffer, 1),
            (CommitStrategy::FromBufferWithLookupTable, 4),
        ]),
        CommitAccountMode::Commit,
    )
    .await;
}

#[tokio::test]
async fn test_commit_5_accounts_1kb_bundle_size_4_undelegate_all() {
    commit_5_accounts_1kb(
        4,
        expect_strategies(&[
            (CommitStrategy::FromBuffer, 1),
            (CommitStrategy::FromBufferWithLookupTable, 4),
        ]),
        CommitAccountMode::CommitAndUndelegate,
    )
    .await;
}

#[tokio::test]
async fn test_commit_5_accounts_1kb_bundle_size_5_undelegate_all() {
    commit_5_accounts_1kb(
        5,
        expect_strategies(&[(CommitStrategy::FromBufferWithLookupTable, 5)]),
        CommitAccountMode::CommitAndUndelegate,
    )
    .await;
}

#[tokio::test]
async fn test_commit_20_accounts_1kb_bundle_size_3() {
    commit_20_accounts_1kb(
        3,
        expect_strategies(&[(CommitStrategy::FromBuffer, 20)]),
    )
    .await;
}

#[tokio::test]
async fn test_commit_20_accounts_1kb_bundle_size_4() {
    commit_20_accounts_1kb(
        4,
        expect_strategies(&[(CommitStrategy::FromBufferWithLookupTable, 20)]),
    )
    .await;
}

#[tokio::test]
async fn test_commit_20_accounts_1kb_bundle_size_6() {
    commit_20_accounts_1kb(
        6,
        expect_strategies(&[
            (CommitStrategy::FromBufferWithLookupTable, 18),
            // Two accounts don't make it into the bundles of size 6
            (CommitStrategy::FromBuffer, 2),
        ]),
    )
    .await;
}

#[tokio::test]
async fn test_commit_20_accounts_1kb_bundle_size_20() {
    commit_20_accounts_1kb(
        20,
        expect_strategies(&[(CommitStrategy::FromBufferWithLookupTable, 20)]),
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
            (CommitStrategy::FromBufferWithLookupTable, 8),
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
            (CommitStrategy::FromBufferWithLookupTable, 20),
        ]),
    )
    .await;
}

// -----------------
// Compressed Account Commits
// -----------------

#[tokio::test]
async fn test_ix_commit_single_compressed_account_100_bytes() {
    commit_single_account(
        100,
        CommitStrategy::Args,
        CommitAccountMode::CompressedCommit,
    )
    .await;
}

#[tokio::test]
async fn test_ix_commit_single_compressed_account_100_bytes_and_undelegate() {
    commit_single_account(
        100,
        CommitStrategy::Args,
        CommitAccountMode::CompressedCommitAndUndelegate,
    )
    .await;
}

#[tokio::test]
async fn test_ix_commit_single_compressed_account_500_bytes() {
    commit_single_account(
        500,
        CommitStrategy::Args,
        CommitAccountMode::CompressedCommit,
    )
    .await;
}

#[tokio::test]
async fn test_ix_commit_single_compressed_account_500_bytes_and_undelegate() {
    commit_single_account(
        500,
        CommitStrategy::Args,
        CommitAccountMode::CompressedCommitAndUndelegate,
    )
    .await;
}

#[tokio::test]
async fn test_ix_commit_two_compressed_accounts_512kb() {
    init_logger!();
    commit_multiple_accounts(
        &[512, 512],
        1,
        CommitAccountMode::CompressedCommit,
        expect_strategies(&[(CommitStrategy::Args, 2)]),
    )
    .await;
}

#[tokio::test]
async fn test_ix_commit_three_compressed_accounts_512kb() {
    init_logger!();
    commit_multiple_accounts(
        &[512, 512, 512],
        1,
        CommitAccountMode::CompressedCommit,
        expect_strategies(&[(CommitStrategy::Args, 3)]),
    )
    .await;
}

#[tokio::test]
async fn test_ix_commit_six_compressed_accounts_512kb() {
    init_logger!();
    commit_multiple_accounts(
        &[512, 512, 512, 512, 512, 512],
        1,
        CommitAccountMode::CompressedCommit,
        expect_strategies(&[(CommitStrategy::Args, 6)]),
    )
    .await;
}

#[tokio::test]
async fn test_commit_20_compressed_accounts_100bytes_bundle_size_2() {
    commit_20_compressed_accounts_100bytes(
        2,
        expect_strategies(&[(CommitStrategy::Args, 20)]),
    )
    .await;
}

#[tokio::test]
async fn test_commit_5_compressed_accounts_100bytes_bundle_size_2() {
    commit_5_compressed_accounts_100bytes(
        2,
        expect_strategies(&[(CommitStrategy::Args, 5)]),
        CommitAccountMode::CompressedCommit,
    )
    .await;
}

#[tokio::test]
async fn test_commit_5_compressed_accounts_100bytes_bundle_size_2_undelegate_all(
) {
    commit_5_compressed_accounts_100bytes(
        2,
        expect_strategies(&[(CommitStrategy::Args, 5)]),
        CommitAccountMode::CompressedCommitAndUndelegate,
    )
    .await;
}

async fn commit_5_accounts_1kb(
    bundle_size: usize,
    expected_strategies: ExpectedStrategies,
    mode_all: CommitAccountMode,
) {
    init_logger!();
    let accs = (0..5).map(|_| 1024).collect::<Vec<_>>();
    commit_multiple_accounts(&accs, bundle_size, mode_all, expected_strategies)
        .await;
}

async fn commit_5_compressed_accounts_100bytes(
    bundle_size: usize,
    expected_strategies: ExpectedStrategies,
    mode_all: CommitAccountMode,
) {
    init_logger!();
    let accs = (0..5).map(|_| 100).collect::<Vec<_>>();
    commit_multiple_accounts(&accs, bundle_size, mode_all, expected_strategies)
        .await;
}

async fn commit_8_accounts_1kb(
    bundle_size: usize,
    expected_strategies: ExpectedStrategies,
) {
    init_logger!();
    let accs = (0..8).map(|_| 1024).collect::<Vec<_>>();
    commit_multiple_accounts(
        &accs,
        bundle_size,
        CommitAccountMode::Commit,
        expected_strategies,
    )
    .await;
}

async fn commit_20_accounts_1kb(
    bundle_size: usize,
    expected_strategies: ExpectedStrategies,
) {
    init_logger!();
    let accs = (0..20).map(|_| 1024).collect::<Vec<_>>();
    commit_multiple_accounts(
        &accs,
        bundle_size,
        CommitAccountMode::Commit,
        expected_strategies,
    )
    .await;
}

async fn commit_20_compressed_accounts_100bytes(
    bundle_size: usize,
    expected_strategies: ExpectedStrategies,
) {
    init_logger!();
    let accs = (0..20).map(|_| 100).collect::<Vec<_>>();
    commit_multiple_accounts(
        &accs,
        bundle_size,
        CommitAccountMode::CompressedCommit,
        expected_strategies,
    )
    .await;
}

async fn create_bundles(
    bundle_size: usize,
    bytess: &[usize],
    compressed: bool,
) -> Vec<Vec<CommittedAccount>> {
    let mut join_set = JoinSet::new();
    for bytes in bytess {
        let bytes = *bytes;
        join_set.spawn(async move {
            let counter_auth = Keypair::new();
            let (pda, mut pda_acc) = if !compressed {
                init_and_delegate_account_on_chain(
                    &counter_auth,
                    bytes as u64,
                    None,
                )
                .await
            } else {
                let (pda, _hash, pda_acc) =
                    init_and_delegate_compressed_account_on_chain(
                        &counter_auth,
                    )
                    .await;
                (pda, pda_acc)
            };

            pda_acc.owner = program_flexi_counter::id();
            pda_acc.data = vec![0u8; bytes];
            CommittedAccount {
                pubkey: pda,
                account: pda_acc,
            }
        });
    }

    // Wait for all tasks to complete
    let committed = join_set.join_all().await;
    committed
        .chunks(bundle_size)
        .map(|chunk| chunk.to_vec())
        .collect()
}

async fn commit_multiple_accounts(
    bytess: &[usize],
    bundle_size: usize,
    mode_all: CommitAccountMode,
    expected_strategies: ExpectedStrategies,
) {
    init_logger!();

    let validator_auth = ensure_validator_authority();
    fund_validator_auth_and_ensure_validator_fees_vault(&validator_auth).await;

    let photon_client =
        Arc::new(PhotonIndexer::new(PHOTON_URL.to_string(), None));

    let service = CommittorService::try_start(
        validator_auth.insecure_clone(),
        ":memory:",
        ChainConfig::local(ComputeBudgetConfig::new(1_000_000)),
        photon_client,
    )
    .unwrap();
    let service = CommittorServiceExt::new(Arc::new(service));

    // Create bundles of committed accounts
    let bundles_of_committees = create_bundles(
        bundle_size,
        bytess,
        matches!(
            mode_all,
            CommitAccountMode::CompressedCommit
                | CommitAccountMode::CompressedCommitAndUndelegate
        ),
    )
    .await;
    // Create intent for each bundle
    let intents = bundles_of_committees
        .into_iter()
        .map(|committees| match mode_all {
            CommitAccountMode::CommitAndUndelegate => {
                MagicBaseIntent::CommitAndUndelegate(CommitAndUndelegate {
                    commit_action: CommitType::Standalone(committees),
                    undelegate_action: UndelegateType::Standalone,
                })
            }
            CommitAccountMode::Commit => {
                MagicBaseIntent::Commit(CommitType::Standalone(committees))
            }
            CommitAccountMode::CompressedCommit => {
                MagicBaseIntent::CompressedCommit(CommitType::Standalone(
                    committees,
                ))
            }
            CommitAccountMode::CompressedCommitAndUndelegate => {
                MagicBaseIntent::CompressedCommitAndUndelegate(
                    CommitAndUndelegate {
                        commit_action: CommitType::Standalone(committees),
                        undelegate_action: UndelegateType::Standalone,
                    },
                )
            }
        })
        .enumerate()
        .map(|(id, base_intent)| ScheduledBaseIntent {
            id: id as u64,
            slot: 0,
            blockhash: Hash::new_unique(),
            action_sent_transaction: Transaction::default(),
            payer: Pubkey::new_unique(),
            base_intent,
        })
        .map(|intent| ScheduledBaseIntentWrapper {
            trigger_type: TriggerType::OnChain,
            inner: intent,
        })
        .collect::<Vec<_>>();

    ix_commit_local(service, intents, expected_strategies).await;
}

// TODO(thlorenz): once delegation program supports larger commits add the following
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
    base_intents: Vec<ScheduledBaseIntentWrapper>,
    expected_strategies: ExpectedStrategies,
) {
    let execution_outputs = service
        .schedule_base_intents_waiting(base_intents.clone())
        .await
        .unwrap()
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .expect("Some commits failed");

    // Assert that all completed
    assert_eq!(execution_outputs.len(), base_intents.len());
    service.release_common_pubkeys().await.unwrap();

    let rpc_client = RpcClient::new(RPC_URL.to_string());
    let photon_indexer = PhotonIndexer::new(PHOTON_URL.to_string(), None);
    let mut strategies = ExpectedStrategies::new();
    for (execution_output, base_intent) in
        execution_outputs.into_iter().zip(base_intents.into_iter())
    {
        let execution_output = execution_output.output;
        let (commit_signature, finalize_signature) = match execution_output {
            ExecutionOutput::SingleStage(signature) => (signature, signature),
            ExecutionOutput::TwoStage {
                commit_signature,
                finalize_signature,
            } => (commit_signature, finalize_signature),
        };

        assert!(
            tx_logs_contain(&rpc_client, &commit_signature, "Commit").await
        );
        assert!(
            tx_logs_contain(&rpc_client, &finalize_signature, "Finalize").await
        );

        let is_undelegate = base_intent.is_undelegate();
        let is_compressed = base_intent.is_compressed();
        if is_undelegate && !is_compressed {
            // Undelegate is part of atomic Finalization Stage
            assert!(
                tx_logs_contain(&rpc_client, &finalize_signature, "Undelegate")
                    .await
            );
        }

        let mut committed_accounts = base_intent
            .get_committed_accounts()
            .unwrap()
            .iter()
            .map(|el| (el.pubkey, el.clone()))
            .collect::<HashMap<Pubkey, CommittedAccount>>();
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

        // When we finalize it is possible to also undelegate the account
        let expected_owner = if is_undelegate {
            program_flexi_counter::id()
        } else {
            dlp::id()
        };

        assert_eq!(statuses.len(), committed_accounts.len());
        for commit_status in statuses {
            if is_compressed {
                let account = committed_accounts
                    .remove(&commit_status.pubkey)
                    .expect("Account should be persisted");
                let lamports = Rent::default().minimum_balance(0);
                get_account!(
                    rpc_client,
                    account.pubkey,
                    "delegated state",
                    |acc: &Account, remaining_tries: u8| {
                        validate_account(
                            acc,
                            remaining_tries,
                            &[],
                            lamports,
                            compressed_delegation_client::ID,
                            account.pubkey,
                            is_undelegate,
                        )
                    }
                );

                let address = derive_cda_from_pda(&account.pubkey);
                let compressed_account = photon_indexer
                    .get_compressed_account(address.to_bytes(), None)
                    .await
                    .unwrap()
                    .value;
                assert!(validate_compressed_account(
                    &compressed_account,
                    &account.account.data,
                    account.account.lamports,
                    program_flexi_counter::id(),
                    account.pubkey,
                    is_undelegate
                ));
            } else {
                let account = committed_accounts
                    .remove(&commit_status.pubkey)
                    .expect("Account should be persisted");
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
                            is_undelegate,
                        )
                    }
                );
            }

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

    if !matches_all && remaining_tries % 4 == 0 {
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

fn validate_compressed_account(
    acc: &CompressedAccount,
    expected_data: &[u8],
    expected_lamports: u64,
    expected_owner: Pubkey,
    account_pubkey: Pubkey,
    is_undelegate: bool,
) -> bool {
    let Some(data) = acc.data.as_ref().and_then(|data| {
        CompressedDelegationRecord::from_bytes(&data.data).ok()
    }) else {
        trace!(
            "Compressed account ({}) data is not present",
            account_pubkey
        );
        return false;
    };
    let matches_data =
        data.data == expected_data && data.lamports == expected_lamports;
    let matches_undelegation = data.owner.eq(&expected_owner);
    let matches_all = matches_data && matches_undelegation;

    if !matches_all {
        if !matches_data {
            trace!(
                "Compressed account ({}) data {} != {} || {} != {}",
                account_pubkey,
                data.data.len(),
                expected_data.len(),
                data.lamports,
                expected_lamports
            );
        }
        if !matches_undelegation {
            trace!(
                "Compressed account ({}) is {} but should be. Owner {} != {}",
                account_pubkey,
                if is_undelegate {
                    "not undelegated"
                } else {
                    "undelegated"
                },
                data.owner,
                expected_owner,
            );
        }
    }
    matches_all
}
