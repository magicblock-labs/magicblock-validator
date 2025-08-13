use log::*;
use magicblock_committor_service::error::CommittorServiceResult;
use magicblock_committor_service::{ChangesetCommittor, ComputeBudgetConfig};
use magicblock_rpc_client::MagicblockRpcClient;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};
use test_tools_core::init_logger;
use tokio::sync::oneshot;
use tokio::task::JoinSet;
use utils::transactions::tx_logs_contain;

use magicblock_committor_program::{ChangedAccount, Changeset};
use magicblock_committor_service::{
    changeset_for_slot,
    config::ChainConfig,
    persist::{CommitStatus, CommitStrategy},
    CommittorService,
};
use solana_account::{Account, AccountSharedData, ReadableAccount};
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_rpc_client_api::config::RpcSendTransactionConfig;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::hash::Hash;
use solana_sdk::transaction::Transaction;
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, signature::Keypair, signer::Signer,
};
use utils::instructions::{
    init_account_and_delegate_ixs, init_validator_fees_vault_ix,
    InitAccountAndDelegateIxs,
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

fn uses_lookup(expected: &ExpectedStrategies) -> bool {
    expected.iter().any(|(strategy, _)| strategy.uses_lookup())
}

macro_rules! get_account {
    ($rpc_client:ident, $pubkey:expr, $label:literal, $predicate:expr) => {{
        const GET_ACCOUNT_RETRIES: u8 = 12;

        let mut remaining_tries = GET_ACCOUNT_RETRIES;
        loop {
            let acc = $rpc_client
                .get_account_with_commitment(
                    &$pubkey,
                    CommitmentConfig::confirmed(),
                )
                .await
                .ok()
                .and_then(|acc| acc.value);
            if let Some(acc) = acc {
                if $predicate(&acc, remaining_tries) {
                    break acc;
                }
                remaining_tries -= 1;
                if remaining_tries == 0 {
                    panic!(
                        "{} account ({}) does not match condition after {} retries",
                        $label, $pubkey, GET_ACCOUNT_RETRIES
                    );
                }
                utils::sleep_millis(800).await;
            } else {
                remaining_tries -= 1;
                if remaining_tries == 0 {
                    panic!(
                        "Unable to get {} account ({}) matching condition after {} retries",
                        $label, $pubkey, GET_ACCOUNT_RETRIES
                    );
                }
                if remaining_tries % 10 == 0 {
                    debug!(
                        "Waiting for {} account ({}) to become available",
                        $label, $pubkey
                    );
                }
                utils::sleep_millis(800).await;
            }
        }
    }};
    ($rpc_client:ident, $pubkey:expr, $label:literal) => {{
        get_account!($rpc_client, $pubkey, $label, |_: &Account, _: u8| true)
    }};
}

/// This needs to be run once for all tests
async fn fund_validator_auth_and_ensure_validator_fees_vault(
    validator_auth: &Keypair,
) {
    let rpc_client = RpcClient::new("http://localhost:7799".to_string());
    rpc_client
        .request_airdrop(&validator_auth.pubkey(), 777 * LAMPORTS_PER_SOL)
        .await
        .unwrap();
    debug!("Airdropped to validator: {} ", validator_auth.pubkey(),);

    let validator_fees_vault_exists = rpc_client
        .get_account(&validator_auth.pubkey())
        .await
        .is_ok();

    if !validator_fees_vault_exists {
        let latest_block_hash =
            rpc_client.get_latest_blockhash().await.unwrap();
        let init_validator_fees_vault_ix =
            init_validator_fees_vault_ix(validator_auth.pubkey());
        // If this fails it might be due to a race condition where another test
        // already initialized it, so we can safely ignore the error
        let _ = rpc_client
            .send_and_confirm_transaction_with_spinner_and_config(
                &Transaction::new_signed_with_payer(
                    &[init_validator_fees_vault_ix],
                    Some(&validator_auth.pubkey()),
                    &[&validator_auth],
                    latest_block_hash,
                ),
                CommitmentConfig::confirmed(),
                RpcSendTransactionConfig {
                    skip_preflight: true,
                    ..Default::default()
                },
            )
            .await
            .map_err(|err| {
                error!("Failed to init validator fees vault: {}", err);
            });
    }
}

/// This needs to be run for each test that required a new counter to be delegated
async fn init_and_delegate_account_on_chain(
    counter_auth: &Keypair,
    bytes: u64,
) -> (Pubkey, Account) {
    let rpc_client = RpcClient::new("http://localhost:7799".to_string());

    rpc_client
        .request_airdrop(&counter_auth.pubkey(), 777 * LAMPORTS_PER_SOL)
        .await
        .unwrap();
    debug!("Airdropped to counter auth: {} SOL", 777 * LAMPORTS_PER_SOL);

    let InitAccountAndDelegateIxs {
        init: init_counter_ix,
        reallocs: realloc_ixs,
        delegate: delegate_ix,
        pda,
        rent_excempt,
    } = init_account_and_delegate_ixs(counter_auth.pubkey(), bytes);

    let latest_block_hash = rpc_client.get_latest_blockhash().await.unwrap();
    // 1. Init account
    rpc_client
        .send_and_confirm_transaction_with_spinner_and_config(
            &Transaction::new_signed_with_payer(
                &[init_counter_ix],
                Some(&counter_auth.pubkey()),
                &[&counter_auth],
                latest_block_hash,
            ),
            CommitmentConfig::confirmed(),
            RpcSendTransactionConfig {
                skip_preflight: true,
                ..Default::default()
            },
        )
        .await
        .expect("Failed to init account");
    debug!("Init account: {:?}", pda);

    // 2. Airdrop to account for extra rent needed for reallocs
    rpc_client
        .request_airdrop(&pda, rent_excempt)
        .await
        .unwrap();

    debug!(
        "Airdropped to account: {:4} {}SOL to pay rent for {} bytes",
        pda,
        rent_excempt as f64 / LAMPORTS_PER_SOL as f64,
        bytes
    );

    // 3. Run reallocs
    for realloc_ix_chunk in realloc_ixs.chunks(10) {
        let tx = Transaction::new_signed_with_payer(
            realloc_ix_chunk,
            Some(&counter_auth.pubkey()),
            &[&counter_auth],
            latest_block_hash,
        );
        rpc_client
            .send_and_confirm_transaction_with_spinner_and_config(
                &tx,
                CommitmentConfig::confirmed(),
                RpcSendTransactionConfig {
                    skip_preflight: true,
                    ..Default::default()
                },
            )
            .await
            .expect("Failed to realloc");
    }
    debug!("Reallocs done");

    // 4. Delegate account
    rpc_client
        .send_and_confirm_transaction_with_spinner_and_config(
            &Transaction::new_signed_with_payer(
                &[delegate_ix],
                Some(&counter_auth.pubkey()),
                &[&counter_auth],
                latest_block_hash,
            ),
            CommitmentConfig::confirmed(),
            RpcSendTransactionConfig {
                skip_preflight: true,
                ..Default::default()
            },
        )
        .await
        .expect("Failed to delegate");
    debug!("Delegated account: {:?}", pda);
    let pda_acc = get_account!(rpc_client, pda, "pda");

    (pda, pda_acc)
}

// -----------------
// +++++ Tests +++++
// -----------------

// -----------------
// Single Account Commits
// -----------------
#[tokio::test]
async fn test_ix_commit_single_account_100_bytes() {
    commit_single_account(100, CommitStrategy::Args, false).await;
}

#[tokio::test]
async fn test_ix_commit_single_account_100_bytes_and_undelegate() {
    commit_single_account(100, CommitStrategy::Args, true).await;
}

#[tokio::test]
async fn test_ix_commit_single_account_800_bytes() {
    commit_single_account(800, CommitStrategy::FromBuffer, false).await;
}

#[tokio::test]
async fn test_ix_commit_single_account_800_bytes_and_undelegate() {
    commit_single_account(800, CommitStrategy::FromBuffer, true).await;
}

#[tokio::test]
async fn test_ix_commit_single_account_one_kb() {
    commit_single_account(1024, CommitStrategy::FromBuffer, false).await;
}
#[tokio::test]
async fn test_ix_commit_single_account_ten_kb() {
    commit_single_account(10 * 1024, CommitStrategy::FromBuffer, false).await;
}

async fn commit_single_account(
    bytes: usize,
    expected_strategy: CommitStrategy,
    undelegate: bool,
) {
    init_logger!();
    let slot = 10;
    let validator_auth = utils::get_validator_auth();

    fund_validator_auth_and_ensure_validator_fees_vault(&validator_auth).await;

    // Run each test with and without finalizing
    for (idx, finalize) in [false, true].into_iter().enumerate() {
        let service = CommittorService::try_start(
            validator_auth.insecure_clone(),
            ":memory:",
            ChainConfig::local(ComputeBudgetConfig::new(1_000_000)),
        )
        .unwrap();

        let (changeset, chain_lamports) = {
            let mut changeset = changeset_for_slot(slot);
            let mut chain_lamports = HashMap::new();
            let counter_auth = Keypair::new();
            let (pda, pda_acc) =
                init_and_delegate_account_on_chain(&counter_auth, bytes as u64)
                    .await;
            let account = Account {
                lamports: LAMPORTS_PER_SOL,
                data: vec![8; bytes],
                owner: program_flexi_counter::id(),
                ..Account::default()
            };
            let account_shared = AccountSharedData::from(account);
            let bundle_id = idx as u64;
            changeset.add(pda, (account_shared, bundle_id));
            if undelegate {
                changeset.request_undelegation(pda);
            }
            chain_lamports.insert(pda, pda_acc.lamports());
            (changeset, chain_lamports)
        };

        ix_commit_local(
            service,
            changeset.clone(),
            chain_lamports.clone(),
            finalize,
            expect_strategies(&[(expected_strategy, 1)]),
        )
        .await;
    }
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
        expect_strategies(&[(CommitStrategy::FromBuffer, 2)]),
        false,
    )
    .await;
}

#[tokio::test]
async fn test_ix_commit_two_accounts_512kb() {
    init_logger!();
    commit_multiple_accounts(
        &[512, 512],
        1,
        expect_strategies(&[(CommitStrategy::Args, 2)]),
        false,
    )
    .await;
}

#[tokio::test]
async fn test_ix_commit_three_accounts_512kb() {
    init_logger!();
    commit_multiple_accounts(
        &[512, 512, 512],
        1,
        expect_strategies(&[(CommitStrategy::Args, 3)]),
        false,
    )
    .await;
}

#[tokio::test]
async fn test_ix_commit_six_accounts_512kb() {
    init_logger!();
    commit_multiple_accounts(
        &[512, 512, 512, 512, 512, 512],
        1,
        expect_strategies(&[(CommitStrategy::Args, 6)]),
        false,
    )
    .await;
}

#[tokio::test]
async fn test_ix_commit_four_accounts_1kb_2kb_5kb_10kb_single_bundle() {
    init_logger!();
    commit_multiple_accounts(
        &[1024, 2 * 1024, 5 * 1024, 10 * 1024],
        1,
        expect_strategies(&[(CommitStrategy::FromBuffer, 4)]),
        false,
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
        false,
    )
    .await;
}

#[tokio::test]
async fn test_commit_5_accounts_1kb_bundle_size_3_undelegate_all() {
    commit_5_accounts_1kb(
        3,
        expect_strategies(&[(CommitStrategy::FromBuffer, 5)]),
        true,
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
        false,
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
        true,
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
        expected_strategies,
        undelegate_all,
    )
    .await;
}

async fn commit_8_accounts_1kb(
    bundle_size: usize,
    expected_strategies: ExpectedStrategies,
) {
    init_logger!();
    let accs = (0..8).map(|_| 1024).collect::<Vec<_>>();
    commit_multiple_accounts(&accs, bundle_size, expected_strategies, false)
        .await;
}

async fn commit_20_accounts_1kb(
    bundle_size: usize,
    expected_strategies: ExpectedStrategies,
) {
    init_logger!();
    let accs = (0..20).map(|_| 1024).collect::<Vec<_>>();
    commit_multiple_accounts(&accs, bundle_size, expected_strategies, false)
        .await;
}

async fn commit_multiple_accounts(
    bytess: &[usize],
    bundle_size: usize,
    expected_strategies: ExpectedStrategies,
    undelegate_all: bool,
) {
    init_logger!();
    let slot = 10;
    let validator_auth = utils::get_validator_auth();

    fund_validator_auth_and_ensure_validator_fees_vault(&validator_auth).await;

    for finalize in [false, true] {
        let mut changeset = changeset_for_slot(slot);

        let service = CommittorService::try_start(
            validator_auth.insecure_clone(),
            ":memory:",
            ChainConfig::local(ComputeBudgetConfig::new(1_000_000)),
        )
        .unwrap();

        let committees =
            bytess.iter().map(|_| Keypair::new()).collect::<Vec<_>>();

        let mut chain_lamports = HashMap::new();
        let expected_strategies = expected_strategies.clone();

        let mut join_set = JoinSet::new();
        let mut bundle_id = 0;

        for (idx, (bytes, counter_auth)) in
            bytess.iter().zip(committees.into_iter()).enumerate()
        {
            if idx % bundle_size == 0 {
                bundle_id += 1;
            }

            let bytes = *bytes;
            join_set.spawn(async move {
                let (pda, pda_acc) = init_and_delegate_account_on_chain(
                    &counter_auth,
                    bytes as u64,
                )
                .await;

                let account = Account {
                    lamports: LAMPORTS_PER_SOL,
                    data: vec![idx as u8; bytes],
                    owner: program_flexi_counter::id(),
                    ..Account::default()
                };
                let account_shared = AccountSharedData::from(account);
                let changed_account =
                    ChangedAccount::from((account_shared, bundle_id as u64));

                // We can only undelegate accounts that are finalized
                let request_undelegation =
                    finalize && (undelegate_all || idx % 2 == 0);
                (
                    pda,
                    pda_acc,
                    changed_account,
                    counter_auth.pubkey(),
                    request_undelegation,
                )
            });
        }

        for (
            pda,
            pda_acc,
            changed_account,
            counter_pubkey,
            request_undelegation,
        ) in join_set.join_all().await
        {
            changeset.add(pda, changed_account);
            if request_undelegation {
                changeset.request_undelegation(counter_pubkey);
            }
            chain_lamports.insert(pda, pda_acc.lamports());
        }

        if uses_lookup(&expected_strategies) {
            let mut join_set = JoinSet::new();
            join_set.spawn(service.reserve_common_pubkeys());
            let owners = changeset.owners();
            for committee in changeset.account_keys().iter() {
                join_set.spawn(map_to_unit(
                    service.reserve_pubkeys_for_committee(
                        **committee,
                        *owners.get(committee).unwrap(),
                    ),
                ));
            }
            debug!(
                "Registering lookup tables for {} committees",
                changeset.account_keys().len()
            );
            join_set.join_all().await;
        }

        ix_commit_local(
            service,
            changeset.clone(),
            chain_lamports.clone(),
            finalize,
            expected_strategies,
        )
        .await;
    }
}

async fn map_to_unit(
    res: oneshot::Receiver<CommittorServiceResult<Instant>>,
) -> Result<CommittorServiceResult<()>, oneshot::error::RecvError> {
    res.await.map(|res| res.map(|_| ()))
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
    service: CommittorService,
    changeset: Changeset,
    chain_lamports: HashMap<Pubkey, u64>,
    finalize: bool,
    expected_strategies: ExpectedStrategies,
) {
    let rpc_client = RpcClient::new("http://localhost:7799".to_string());

    let ephemeral_blockhash = Hash::default();
    let reqid = service
        .commit_changeset(changeset.clone(), ephemeral_blockhash, finalize)
        .await
        .unwrap()
        .unwrap();
    let statuses = service.get_commit_statuses(reqid).await.unwrap().unwrap();
    service.release_common_pubkeys().await.unwrap();

    debug!(
        "{}",
        statuses
            .iter()
            .map(|x| x.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert_eq!(statuses.len(), changeset.accounts.len());
    CommitStatus::all_completed(
        &statuses
            .iter()
            .map(|x| x.commit_status.clone())
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let mut strategies = ExpectedStrategies::new();
    for res in statuses {
        let change = changeset.accounts.get(&res.pubkey).cloned().unwrap();
        let lamports = if finalize {
            change.lamports()
        } else {
            // The commit state account will hold only the lamports needed
            // to be rent exempt and debit the delegated account to reach the
            // lamports of the account as changed in the ephemeral
            change.lamports() - chain_lamports[&res.pubkey]
        };

        // Track the strategy used
        let strategy = res.commit_status.commit_strategy();
        let strategy_count = strategies.entry(strategy).or_insert(0);
        *strategy_count += 1;

        // Ensure that the signatures are pointing to the correct transactions
        let signatures =
            res.commit_status.signatures().expect("Missing signatures");

        assert!(
            tx_logs_contain(
                &rpc_client,
                &signatures.process_signature,
                "CommitState"
            )
            .await
        );

        // If we finalized the commit then the delegate account should have the
        // committed state, otherwise it is still held in the commit state account
        // NOTE: that we verify data/lamports via the get_account! condition
        if finalize {
            assert!(
                signatures.finalize_signature.is_some(),
                "Missing finalize signature"
            );
            assert!(
                tx_logs_contain(
                    &rpc_client,
                    &signatures.finalize_signature.unwrap(),
                    "Finalize"
                )
                .await
            );
            if res.undelegate {
                assert!(
                    signatures.undelegate_signature.is_some(),
                    "Missing undelegate signature"
                );
                assert!(
                    tx_logs_contain(
                        &rpc_client,
                        &signatures.undelegate_signature.unwrap(),
                        "Undelegate"
                    )
                    .await
                );
            }
            get_account!(
                rpc_client,
                res.pubkey,
                "delegated state",
                |acc: &Account, remaining_tries: u8| {
                    let matches_data = acc.data() == change.data()
                        && acc.lamports() == lamports;
                    // When we finalize it is possible to also undelegate the account
                    let expected_owner = if res.undelegate {
                        program_flexi_counter::id()
                    } else {
                        dlp::id()
                    };
                    let matches_undelegation = acc.owner().eq(&expected_owner);
                    let matches_all = matches_data && matches_undelegation;

                    if !matches_all && remaining_tries % 4 == 0 {
                        if !matches_data {
                            trace!(
                                "Account ({}) data {} != {} || {} != {}",
                                res.pubkey,
                                acc.data().len(),
                                change.data().len(),
                                acc.lamports(),
                                lamports
                            );
                        }
                        if !matches_undelegation {
                            trace!(
                                "Account ({}) is {} but should be. Owner {} != {}",
                                res.pubkey,
                                if res.undelegate {
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
            )
        } else {
            let commit_state_pda =
                dlp::pda::commit_state_pda_from_delegated_account(&res.pubkey);
            get_account!(
                rpc_client,
                commit_state_pda,
                "commit state",
                |acc: &Account, remaining_tries: u8| {
                    if remaining_tries % 4 == 0 {
                        trace!(
                            "Commit state ({}) {} == {}? {} == {}?",
                            commit_state_pda,
                            acc.data().len(),
                            change.data().len(),
                            acc.lamports(),
                            lamports
                        );
                    }
                    acc.data() == change.data() && acc.lamports() == lamports
                }
            )
        };
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
