use dlp::pda::ephemeral_balance_pda_from_payer;
use integration_test_tools::IntegrationTestContext;
use program_flexi_counter::{
    delegation_program_id,
    instruction::{
        create_add_ix, create_delegate_ix, create_init_ix,
        create_intent_bundle_ix, create_intent_ix,
    },
    state::FlexiCounter,
};
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, pubkey::Pubkey, rent::Rent,
    signature::Keypair, signer::Signer, transaction::Transaction,
};
use test_kit::init_logger;
use tracing::*;

const LABEL: &str = "I am a label";

#[test]
fn test_schedule_intent_basic() {
    // Init context
    let ctx = IntegrationTestContext::try_new().unwrap();
    // Payer to fund all transactions on chain
    let payer = setup_payer(&ctx);

    // Init counter
    init_counter(&ctx, &payer);
    // Delegate counter
    delegate_counter(&ctx, &payer);
    add_to_counter(&ctx, &payer, 101);

    schedule_intent(
        &ctx,
        &[&payer],
        None,
        // We cannot wait that long in a test ever, so this option was removed
        // Some(Duration::from_secs(10)),
    );

    // Assert that 101 value got committed from ER to base
    assert_counters(
        &ctx,
        &[ExpectedCounter {
            pda: FlexiCounter::pda(&payer.pubkey()).0,
            expected: 101,
        }],
        true,
    );
}

#[test]
fn test_schedule_intent_and_undelegate() {
    // Init context
    let ctx = IntegrationTestContext::try_new().unwrap();
    // Payer to fund all transactions on chain
    let payer = setup_payer(&ctx);

    // Init counter
    init_counter(&ctx, &payer);
    // Delegate counter
    delegate_counter(&ctx, &payer);
    add_to_counter(&ctx, &payer, 101);

    schedule_intent(&ctx, &[&payer], Some(vec![-100]));
    // Assert that action after undelegate subtracted 100 from 101
    let pda = FlexiCounter::pda(&payer.pubkey()).0;
    assert_counters(&ctx, &[ExpectedCounter { pda, expected: 1 }], true);

    verify_undelegation_in_ephem_via_owner(&[payer.pubkey()], &ctx);
}

#[test]
fn test_schedule_intent_2_commits() {
    // Init context
    let ctx = IntegrationTestContext::try_new().unwrap();
    let payer = setup_payer(&ctx);

    // Init counter
    init_counter(&ctx, &payer);
    // Delegate counter
    delegate_counter(&ctx, &payer);
    add_to_counter(&ctx, &payer, 101);

    schedule_intent(&ctx, &[&payer], None);
    assert_counters(
        &ctx,
        &[ExpectedCounter {
            pda: FlexiCounter::pda(&payer.pubkey()).0,
            expected: 101,
        }],
        true,
    );

    add_to_counter(&ctx, &payer, 2);
    schedule_intent(&ctx, &[&payer], None);
    assert_counters(
        &ctx,
        &[ExpectedCounter {
            pda: FlexiCounter::pda(&payer.pubkey()).0,
            expected: 103,
        }],
        true,
    );
}

#[test]
fn test_schedule_intent_undelegate_delegate_back_undelegate_again() {
    // Init context
    let ctx = IntegrationTestContext::try_new().unwrap();
    let payer = setup_payer(&ctx);

    // Init counter
    init_counter(&ctx, &payer);
    // Delegate counter
    delegate_counter(&ctx, &payer);
    add_to_counter(&ctx, &payer, 101);

    schedule_intent(&ctx, &[&payer], Some(vec![-100]));
    assert_counters(
        &ctx,
        &[ExpectedCounter {
            pda: FlexiCounter::pda(&payer.pubkey()).0,
            expected: 1,
        }],
        true,
    );

    verify_undelegation_in_ephem_via_owner(&[payer.pubkey()], &ctx);

    // Delegate back
    delegate_counter(&ctx, &payer);
    schedule_intent(&ctx, &[&payer], Some(vec![102]));
    assert_counters(
        &ctx,
        &[ExpectedCounter {
            pda: FlexiCounter::pda(&payer.pubkey()).0,
            expected: 103,
        }],
        true,
    );
}

#[test]
fn test_2_payers_intent_with_undelegation() {
    init_logger!();
    const PAYERS: usize = 2;

    // Init context
    let ctx = IntegrationTestContext::try_new().unwrap();
    let payers = (0..PAYERS).map(|_| setup_payer(&ctx)).collect::<Vec<_>>();
    debug!("✅ Airdropped to payers on chain with escrow");

    // Init and setup counters for each payer
    let values: [u8; PAYERS] = [100, 200];
    for (idx, payer) in payers.iter().enumerate() {
        // Init counter on chain and delegate it to ephemeral
        init_counter(&ctx, payer);
        delegate_counter(&ctx, payer);
        debug!(
            "✅ Initialized and delegated counter for payer {}",
            payer.pubkey()
        );

        // Add to counter in ephemeral
        add_to_counter(&ctx, payer, values[idx]);
        debug!("✅ Added to counter for payer {}", payer.pubkey());
    }

    // Schedule intent affecting all counters
    schedule_intent(
        &ctx,
        payers.iter().collect::<Vec<&Keypair>>().as_slice(),
        Some(vec![-50, 25]),
    );
    debug!("✅ Scheduled intent for all payers");

    assert_counters(
        &ctx,
        &[
            ExpectedCounter {
                pda: FlexiCounter::pda(&payers[0].pubkey()).0,
                expected: 50,
            },
            ExpectedCounter {
                pda: FlexiCounter::pda(&payers[1].pubkey()).0,
                expected: 225,
            },
        ],
        true,
    );
    debug!("✅ Verified counters on base layer");

    verify_undelegation_in_ephem_via_owner(
        &payers.iter().map(|p| p.pubkey()).collect::<Vec<_>>(),
        &ctx,
    );
}

#[test]
fn test_1_payers_intent_with_undelegation() {
    init_logger!();
    const PAYERS: usize = 1;

    // Init context
    let ctx = IntegrationTestContext::try_new().unwrap();
    let payers = (0..PAYERS).map(|_| setup_payer(&ctx)).collect::<Vec<_>>();
    debug!("✅ Airdropped to payers on chain with escrow");

    // Init and setup counters for each payer
    let values: [u8; PAYERS] = [100];
    for (idx, payer) in payers.iter().enumerate() {
        // Init counter on chain and delegate it to ephemeral
        init_counter(&ctx, payer);
        delegate_counter(&ctx, payer);
        debug!(
            "✅ Initialized and delegated counter for payer {}",
            payer.pubkey()
        );

        // Add to counter in ephemeral
        add_to_counter(&ctx, payer, values[idx]);
        debug!("✅ Added to counter for payer {}", payer.pubkey());
    }

    // Schedule intent affecting all counters
    schedule_intent(
        &ctx,
        payers.iter().collect::<Vec<&Keypair>>().as_slice(),
        Some(vec![-50]),
    );
    debug!("✅ Scheduled intent for all payers");

    assert_counters(
        &ctx,
        &[ExpectedCounter {
            pda: FlexiCounter::pda(&payers[0].pubkey()).0,
            expected: 50,
        }],
        true,
    );
    debug!("✅ Verified counters on base layer");

    verify_undelegation_in_ephem_via_owner(
        &payers.iter().map(|p| p.pubkey()).collect::<Vec<_>>(),
        &ctx,
    );
    debug!("✅ Verified undelegation via account owner");
}

#[ignore = "With sdk having ShortAccountMetas instead of u8s we hit limited_deserialize here as instruction exceeds 1232 bytes"]
#[test]
fn test_5_payers_intent_only_commit() {
    const PAYERS: usize = 5;

    // Init context
    let ctx = IntegrationTestContext::try_new().unwrap();
    let payers = (0..PAYERS).map(|_| setup_payer(&ctx)).collect::<Vec<_>>();

    // Init and setup counters for each payer
    let values: [u8; PAYERS] = std::array::from_fn(|i| 180 + i as u8);
    payers.iter().enumerate().for_each(|(i, payer)| {
        init_counter(&ctx, payer);
        delegate_counter(&ctx, payer);
        add_to_counter(&ctx, payer, values[i]);
    });

    let counter_diffs: [i64; PAYERS] = [-2; PAYERS];
    // Schedule intent affecting all counters
    schedule_intent(
        &ctx,
        payers.iter().collect::<Vec<&Keypair>>().as_slice(),
        Some(counter_diffs.to_vec()),
    );
}

// This isn't enabled at this point due to solana reentrancy restriction
// We have DLP calling program USER, and USER calling delegate in DLP
// Solana prohibits this
#[ignore = "Redelegation blocked by Solana reentrancy restrictions"]
#[test]
fn test_redelegation_intent() {
    // Init context
    let ctx = IntegrationTestContext::try_new().unwrap();
    let payer = setup_payer(&ctx);

    // Init counter
    init_counter(&ctx, &payer);
    // Delegate counter
    delegate_counter(&ctx, &payer);
    add_to_counter(&ctx, &payer, 101);
    // redelegate_intent(&ctx, &payer);
}

/// Tests the new MagicIntentBundleBuilder feature where a single IntentBundle
/// can contain both Commit and CommitAndUndelegate intents simultaneously.
///
/// Setup:
/// - 2 payers for commit-only (their counters will just be committed)
/// - 2 payers for commit-and-undelegate (their counters will be committed and undelegated)
///
/// Expected behavior:
/// - All 4 counters should be committed to base layer
/// - Only the undelegate payers' counters should be undelegated
/// - Commit-only payers' counters remain delegated
#[test]
fn test_intent_bundle_commit_and_undelegate_simultaneously() {
    init_logger!();

    // Init context
    let ctx = IntegrationTestContext::try_new().unwrap();

    // Create 2 payers for commit-only and 1 for undelegate
    let commit_only_payers: Vec<Keypair> =
        (0..2).map(|_| setup_payer(&ctx)).collect();
    let undelegate_payer = setup_payer(&ctx);

    debug!(
        "Created {} commit-only payers and 1 undelegate payer",
        commit_only_payers.len()
    );

    // Init and delegate counters for commit-only payers
    let commit_values: [u8; 2] = [50, 75];
    for (idx, payer) in commit_only_payers.iter().enumerate() {
        init_counter(&ctx, payer);
        delegate_counter(&ctx, payer);
        add_to_counter(&ctx, payer, commit_values[idx]);
        debug!(
            "Commit-only payer {} initialized with value {}",
            payer.pubkey(),
            commit_values[idx]
        );
    }

    // Init and delegate counter for undelegate payer
    let undelegate_value: u8 = 100;
    init_counter(&ctx, &undelegate_payer);
    delegate_counter(&ctx, &undelegate_payer);
    add_to_counter(&ctx, &undelegate_payer, undelegate_value);
    debug!(
        "Undelegate payer {} initialized with value {}",
        undelegate_payer.pubkey(),
        undelegate_value
    );

    // Schedule intent bundle with both Commit and CommitAndUndelegate
    // Counter diff: -10 for undelegate payer
    let counter_diffs = vec![-10i64];
    schedule_intent_bundle(
        &ctx,
        &commit_only_payers.iter().collect::<Vec<_>>(),
        &[&undelegate_payer],
        counter_diffs,
    );
    debug!("Scheduled intent bundle");

    // Assert commit-only counters have their values committed
    // (commit-only payers: 50, 75)
    assert_counters(
        &ctx,
        &[
            ExpectedCounter {
                pda: FlexiCounter::pda(&commit_only_payers[0].pubkey()).0,
                expected: 50,
            },
            ExpectedCounter {
                pda: FlexiCounter::pda(&commit_only_payers[1].pubkey()).0,
                expected: 75,
            },
        ],
        true,
    );
    debug!("Verified commit-only counters on base layer");

    // Assert undelegate counter has its value committed + counter_diff applied
    // (undelegate payer: 100 + (-10) = 90)
    assert_counters(
        &ctx,
        &[ExpectedCounter {
            pda: FlexiCounter::pda(&undelegate_payer.pubkey()).0,
            expected: 90,
        }],
        true,
    );
    debug!("Verified undelegate counter on base layer");

    // Verify that only undelegate payer's account is undelegated
    verify_undelegation_in_ephem_via_owner(&[undelegate_payer.pubkey()], &ctx);
    debug!("Verified undelegation for undelegate payer");

    // Verify that commit-only payers' accounts are still delegated
    for payer in &commit_only_payers {
        let counter_pda = FlexiCounter::pda(&payer.pubkey()).0;
        let owner = ctx.fetch_chain_account_owner(counter_pda).unwrap();
        assert_eq!(
            owner,
            delegation_program_id(),
            "Commit-only counter should still be delegated"
        );
    }
    debug!("Verified commit-only counters are still delegated");
}

/// Tests IntentBundle with only Commit intent (no undelegate).
/// This ensures the new API works for commit-only scenarios.
#[test]
fn test_intent_bundle_commit_only() {
    init_logger!();

    let ctx = IntegrationTestContext::try_new().unwrap();

    let commit_only_payers: Vec<Keypair> =
        (0..2).map(|_| setup_payer(&ctx)).collect();

    // Init and delegate counters
    let values: [u8; 2] = [42, 88];
    for (idx, payer) in commit_only_payers.iter().enumerate() {
        init_counter(&ctx, payer);
        delegate_counter(&ctx, payer);
        add_to_counter(&ctx, payer, values[idx]);
    }

    // Schedule bundle with only commit intent (no undelegate payers)
    schedule_intent_bundle(
        &ctx,
        &commit_only_payers.iter().collect::<Vec<_>>(),
        &[], // No undelegate payers
        vec![],
    );

    // Verify commits
    assert_counters(
        &ctx,
        &[
            ExpectedCounter {
                pda: FlexiCounter::pda(&commit_only_payers[0].pubkey()).0,
                expected: 42,
            },
            ExpectedCounter {
                pda: FlexiCounter::pda(&commit_only_payers[1].pubkey()).0,
                expected: 88,
            },
        ],
        true,
    );

    // Verify still delegated
    for payer in &commit_only_payers {
        let counter_pda = FlexiCounter::pda(&payer.pubkey()).0;
        let owner = ctx.fetch_chain_account_owner(counter_pda).unwrap();
        assert_eq!(owner, delegation_program_id());
    }
}

/// Tests IntentBundle with only CommitAndUndelegate intent (no commit-only).
/// This ensures the new API works for undelegate-only scenarios.
#[test]
fn test_intent_bundle_undelegate_only() {
    init_logger!();

    let ctx = IntegrationTestContext::try_new().unwrap();

    let undelegate_payers: Vec<Keypair> =
        (0..2).map(|_| setup_payer(&ctx)).collect();

    // Init and delegate counters
    let values: [u8; 2] = [200, 250];
    for (idx, payer) in undelegate_payers.iter().enumerate() {
        init_counter(&ctx, payer);
        delegate_counter(&ctx, payer);
        add_to_counter(&ctx, payer, values[idx]);
    }

    // Schedule bundle with only undelegate intent (no commit-only payers)
    let counter_diffs = vec![50i64, -100i64];
    schedule_intent_bundle(
        &ctx,
        &[], // No commit-only payers
        &undelegate_payers.iter().collect::<Vec<_>>(),
        counter_diffs,
    );

    // Verify values (200 + 50 = 250, 250 - 100 = 150)
    assert_counters(
        &ctx,
        &[
            ExpectedCounter {
                pda: FlexiCounter::pda(&undelegate_payers[0].pubkey()).0,
                expected: 250,
            },
            ExpectedCounter {
                pda: FlexiCounter::pda(&undelegate_payers[1].pubkey()).0,
                expected: 150,
            },
        ],
        true,
    );

    // Verify undelegation
    verify_undelegation_in_ephem_via_owner(
        &undelegate_payers
            .iter()
            .map(|p| p.pubkey())
            .collect::<Vec<_>>(),
        &ctx,
    );
}

fn setup_payer(ctx: &IntegrationTestContext) -> Keypair {
    // Airdrop to payer on chain
    let payer = Keypair::new();
    ctx.airdrop_chain(&payer.pubkey(), LAMPORTS_PER_SOL)
        .unwrap();

    // Create actor escrow
    let ix = dlp::instruction_builder::top_up_ephemeral_balance(
        payer.pubkey(),
        payer.pubkey(),
        Some(LAMPORTS_PER_SOL / 2),
        Some(1),
    );
    ctx.send_and_confirm_instructions_with_payer_chain(&[ix], &payer)
        .unwrap();

    // Confirm actor escrow
    let escrow_pda = ephemeral_balance_pda_from_payer(&payer.pubkey(), 1);
    let rent = Rent::default().minimum_balance(0);
    assert_eq!(
        ctx.fetch_chain_account(escrow_pda).unwrap().lamports,
        LAMPORTS_PER_SOL / 2 + rent
    );

    payer
}

fn init_counter(ctx: &IntegrationTestContext, payer: &Keypair) {
    let ix = create_init_ix(payer.pubkey(), LABEL.to_string());
    let (_, confirmed) = ctx
        .send_and_confirm_instructions_with_payer_chain(&[ix], payer)
        .unwrap();
    assert!(confirmed, "Should confirm transaction");

    let counter_pda = FlexiCounter::pda(&payer.pubkey()).0;
    let counter = ctx
        .fetch_chain_account_struct::<FlexiCounter>(counter_pda)
        .unwrap();
    assert_eq!(
        counter,
        FlexiCounter {
            count: 0,
            updates: 0,
            label: LABEL.to_string()
        },
    )
}

// ER action
fn delegate_counter(ctx: &IntegrationTestContext, payer: &Keypair) {
    ctx.wait_for_next_slot_ephem().unwrap();

    let counter_pda = FlexiCounter::pda(&payer.pubkey()).0;
    let ix = create_delegate_ix(payer.pubkey());
    ctx.send_and_confirm_instructions_with_payer_chain(&[ix], payer)
        .unwrap();

    // Confirm delegated
    let owner = ctx.fetch_chain_account_owner(counter_pda).unwrap();
    assert_eq!(owner, delegation_program_id());
}

// ER action
fn add_to_counter(ctx: &IntegrationTestContext, payer: &Keypair, value: u8) {
    ctx.wait_for_next_slot_ephem().unwrap();

    let counter_pda = FlexiCounter::pda(&payer.pubkey()).0;
    let counter_before = ctx
        .fetch_ephem_account_struct::<FlexiCounter>(counter_pda)
        .unwrap_or(FlexiCounter {
            count: 0,
            updates: 0,
            label: LABEL.to_string(),
        });

    // Add value to counter
    let ix = create_add_ix(payer.pubkey(), value);
    ctx.send_and_confirm_instructions_with_payer_ephem(&[ix], payer)
        .unwrap();

    let counter = ctx
        .fetch_ephem_account_struct::<FlexiCounter>(counter_pda)
        .unwrap();
    assert_eq!(
        counter,
        FlexiCounter {
            count: counter_before.count + value as u64,
            updates: counter_before.updates + 1,
            label: LABEL.to_string()
        },
    )
}

struct ExpectedCounter {
    pda: Pubkey,
    expected: u64,
}

fn assert_counters(
    ctx: &IntegrationTestContext,
    expected_counters: &[ExpectedCounter],
    is_base: bool,
) {
    // Confirm results on base lauer
    let actual_counter = expected_counters
        .iter()
        .map(|counter| {
            if is_base {
                ctx.fetch_chain_account_struct::<FlexiCounter>(counter.pda)
                    .unwrap()
            } else {
                ctx.fetch_ephem_account_struct::<FlexiCounter>(counter.pda)
                    .unwrap()
            }
        })
        .collect::<Vec<_>>();

    for i in 0..actual_counter.len() {
        let actual_counter = &actual_counter[i];
        let expected_counter = &expected_counters[i];
        assert_eq!(actual_counter.count, expected_counter.expected);
    }
}

fn schedule_intent(
    ctx: &IntegrationTestContext,
    payers: &[&Keypair],
    counter_diffs: Option<Vec<i64>>,
) {
    ctx.wait_for_next_slot_ephem().unwrap();

    let transfer_destination = Keypair::new();
    let payers_pubkeys = payers.iter().map(|payer| payer.pubkey()).collect();
    let ix = create_intent_ix(
        payers_pubkeys,
        transfer_destination.pubkey(),
        counter_diffs.clone(),
        100_000,
    );

    let mut tx = Transaction::new_with_payer(&[ix], Some(&payers[0].pubkey()));
    let (sig, confirmed) = ctx
        .send_and_confirm_transaction_ephem(&mut tx, payers)
        .unwrap();
    assert!(confirmed);

    // Confirm was sent on Base Layer
    let commit_result = ctx
        .fetch_schedule_commit_result::<FlexiCounter>(sig)
        .unwrap();
    commit_result
        .confirm_commit_transactions_on_chain(ctx)
        .unwrap();

    // ensure Prize = 1_000_000 is transferred
    let transfer_destination_balance = ctx
        .fetch_chain_account_balance(&transfer_destination.pubkey())
        .unwrap();

    let mutiplier = if counter_diffs.is_some() { 2 } else { 1 };
    assert_eq!(
        transfer_destination_balance,
        mutiplier * payers.len() as u64 * 1_000_000
    );
}

/// Schedule an intent bundle using the new MagicIntentBundleBuilder API.
/// This creates an IntentBundle that can contain both Commit and CommitAndUndelegate intents.
fn schedule_intent_bundle(
    ctx: &IntegrationTestContext,
    commit_only_payers: &[&Keypair],
    undelegate_payers: &[&Keypair],
    counter_diffs: Vec<i64>,
) {
    ctx.wait_for_next_slot_ephem().unwrap();

    let transfer_destination = Keypair::new();
    let commit_only_pubkeys: Vec<Pubkey> =
        commit_only_payers.iter().map(|p| p.pubkey()).collect();
    let undelegate_pubkeys: Vec<Pubkey> =
        undelegate_payers.iter().map(|p| p.pubkey()).collect();

    let ix = create_intent_bundle_ix(
        commit_only_pubkeys,
        undelegate_pubkeys,
        transfer_destination.pubkey(),
        counter_diffs,
        100_000,
    );

    // Collect all signers - need at least one
    let all_payers: Vec<&Keypair> = commit_only_payers
        .iter()
        .chain(undelegate_payers.iter())
        .copied()
        .collect();

    assert!(
        !all_payers.is_empty(),
        "At least one payer required for intent bundle"
    );

    let mut tx =
        Transaction::new_with_payer(&[ix], Some(&all_payers[0].pubkey()));
    let (sig, confirmed) = ctx
        .send_and_confirm_transaction_ephem(&mut tx, &all_payers)
        .unwrap();
    assert!(confirmed);

    // Confirm was sent on Base Layer
    let commit_result = ctx
        .fetch_schedule_commit_result::<FlexiCounter>(sig)
        .unwrap();
    commit_result
        .confirm_commit_transactions_on_chain(ctx)
        .unwrap();

    // Verify Prize = 1_000_000 is transferred for each action
    // - commit-only payers: 1 action each (commit)
    // - undelegate payers: 2 actions each (commit + undelegate)
    let transfer_destination_balance = ctx
        .fetch_chain_account_balance(&transfer_destination.pubkey())
        .unwrap();

    let expected_balance = (commit_only_payers.len() as u64 * 1_000_000)
        + (undelegate_payers.len() as u64 * 2 * 1_000_000);
    assert_eq!(transfer_destination_balance, expected_balance);
}

fn verify_undelegation_in_ephem_via_owner(
    pubkeys: &[Pubkey],
    ctx: &IntegrationTestContext,
) {
    const RETRY_LIMIT: usize = 20;
    let mut retries = 0;

    loop {
        ctx.wait_for_next_slot_ephem().unwrap();
        let mut not_verified = vec![];
        for pk in pubkeys.iter() {
            let counter_pda = FlexiCounter::pda(pk).0;
            let owner = ctx.fetch_ephem_account_owner(counter_pda).unwrap();
            if owner == delegation_program_id() {
                not_verified.push(*pk);
            }
        }
        if not_verified.is_empty() {
            break;
        }
        retries += 1;
        if retries >= RETRY_LIMIT {
            panic!(
                "Failed to verify undelegation for pubkeys: {}",
                not_verified
                    .iter()
                    .map(|k| k.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
}
