use dlp::pda::ephemeral_balance_pda_from_payer;
use integration_test_tools::IntegrationTestContext;
use log::*;
use program_flexi_counter::{
    delegation_program_id,
    instruction::{
        create_add_ix, create_delegate_ix, create_init_ix, create_intent_ix,
    },
    state::FlexiCounter,
};
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, pubkey::Pubkey, rent::Rent,
    signature::Keypair, signer::Signer, transaction::Transaction,
};
use test_kit::init_logger;

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
    assert_counters(
        &ctx,
        &[ExpectedCounter {
            pda: FlexiCounter::pda(&payer.pubkey()).0,
            expected: 1,
        }],
        true,
    );
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
