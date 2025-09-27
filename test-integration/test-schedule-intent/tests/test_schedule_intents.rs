use std::time::Duration;

use dlp::pda::ephemeral_balance_pda_from_payer;
use integration_test_tools::IntegrationTestContext;
use program_flexi_counter::{
    delegation_program_id,
    instruction::{
        create_add_ix, create_delegate_ix, create_init_ix, create_intent_ix,
        create_redelegation_intent_ix,
    },
    state::FlexiCounter,
};
use solana_rpc_client_api::config::RpcSendTransactionConfig;
use solana_sdk::{
    commitment_config::CommitmentConfig, native_token::LAMPORTS_PER_SOL,
    rent::Rent, signature::Keypair, signer::Signer, transaction::Transaction,
};

const LABEL: &str = "I am a label";

#[test]
fn test_schedule_intent() {
    // Init context
    let ctx = IntegrationTestContext::try_new().unwrap();
    let payer = setup_payer(&ctx);

    // Init counter
    init_counter(&ctx, &payer);
    // Delegate counter
    delegate_counter(&ctx, &payer);
    add_to_counter(&ctx, &payer, 101);
    schedule_intent(
        &ctx,
        &[&payer],
        vec![-100],
        false,
        Some(Duration::from_secs(10)),
    );
}

#[test]
fn test_schedule_intent_and_undelegate() {
    // Init context
    let ctx = IntegrationTestContext::try_new().unwrap();
    let payer = setup_payer(&ctx);

    // Init counter
    init_counter(&ctx, &payer);
    // Delegate counter
    delegate_counter(&ctx, &payer);
    add_to_counter(&ctx, &payer, 101);
    schedule_intent(&ctx, &[&payer], vec![-100], true, None);
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
    schedule_intent(&ctx, &[&payer], vec![-100], false, None);
    schedule_intent(&ctx, &[&payer], vec![-100], false, None);
}

#[test]
fn test_3_payers_intent_with_undelegation() {
    const PAYERS: usize = 3;

    // Init context
    let ctx = IntegrationTestContext::try_new().unwrap();
    let payers = (0..PAYERS).map(|_| setup_payer(&ctx)).collect::<Vec<_>>();

    // Init and setup counters for each payer
    let values: [u8; PAYERS] = [100, 200, 201];
    payers.iter().enumerate().for_each(|(i, payer)| {
        init_counter(&ctx, payer);
        delegate_counter(&ctx, payer);
        add_to_counter(&ctx, payer, values[i]);
    });

    // Schedule intent affecting all counters
    schedule_intent(
        &ctx,
        payers.iter().collect::<Vec<&Keypair>>().as_slice(),
        vec![-50, 25, -75],
        true,
        Some(Duration::from_secs(50)),
    );
}

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
        counter_diffs.to_vec(),
        true,
        Some(Duration::from_secs(20)),
    );
}

#[ignore = "Will be enabled once MagicProgram support overrides of AccountMeta. Followup PR"]
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
    redelegate_intent(&ctx, &payer);
}

fn setup_payer(ctx: &IntegrationTestContext) -> Keypair {
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

fn schedule_intent(
    ctx: &IntegrationTestContext,
    payers: &[&Keypair],
    counter_diffs: Vec<i64>,
    is_undelegate: bool,
    confirmation_wait: Option<Duration>,
) {
    ctx.wait_for_next_slot_ephem().unwrap();

    let counters_before = payers
        .iter()
        .map(|payer| {
            let counter_pda = FlexiCounter::pda(&payer.pubkey()).0;
            ctx.fetch_ephem_account_struct::<FlexiCounter>(counter_pda)
                .unwrap()
        })
        .collect::<Vec<_>>();

    let transfer_destination = Keypair::new();
    let payers_pubkeys = payers.iter().map(|payer| payer.pubkey()).collect();
    let ix = create_intent_ix(
        payers_pubkeys,
        transfer_destination.pubkey(),
        counter_diffs.clone(),
        is_undelegate,
        100_000,
    );

    let rpc_client = ctx.try_ephem_client().unwrap();
    let blockhash = rpc_client.get_latest_blockhash().unwrap();
    let tx = Transaction::new_signed_with_payer(&[ix], None, payers, blockhash);
    let sig = rpc_client
        .send_transaction_with_config(
            &tx,
            RpcSendTransactionConfig {
                skip_preflight: true,
                ..Default::default()
            },
        )
        .unwrap();

    println!("sigtrtr: {}", sig);
    // In some cases it takes longer for tx to make it to baselayer
    // we need an additional wait time
    if let Some(confirmation_wait) = confirmation_wait {
        std::thread::sleep(confirmation_wait);
    }
    let confirmed = IntegrationTestContext::confirm_transaction(
        &sig,
        rpc_client,
        CommitmentConfig::confirmed(),
        Some(&tx),
    )
    .unwrap();
    assert!(confirmed);

    // Confirm was sent on Base Layer
    let commit_result = ctx
        .fetch_schedule_commit_result::<FlexiCounter>(sig)
        .unwrap();
    commit_result
        .confirm_commit_transactions_on_chain(ctx)
        .unwrap();

    // Confirm results on base lauer
    let counters_after = payers
        .iter()
        .map(|payer| {
            let counter_pda = FlexiCounter::pda(&payer.pubkey()).0;
            ctx.fetch_chain_account_struct::<FlexiCounter>(counter_pda)
                .unwrap()
        })
        .collect::<Vec<_>>();

    for i in 0..counter_diffs.len() {
        let counter_before = &counters_before[i];
        let counter_after = &counters_after[i];
        let counter_diff = counter_diffs[i];
        if is_undelegate {
            assert_eq!(
                counter_before.count as i64 + counter_diff,
                counter_after.count as i64
            )
        } else {
            assert_eq!(counter_before.count, counter_after.count)
        }
    }

    // ensure Prize = 1_000_000 is transferred
    let transfer_destination_balance = ctx
        .fetch_chain_account_balance(&transfer_destination.pubkey())
        .unwrap();

    let mutiplier = if is_undelegate { 2 } else { 1 };
    assert_eq!(
        transfer_destination_balance,
        mutiplier * payers.len() as u64 * 1_000_000
    );
}

fn redelegate_intent(ctx: &IntegrationTestContext, payer: &Keypair) {
    ctx.wait_for_next_slot_ephem().unwrap();

    let (pda, _) = FlexiCounter::pda(&payer.pubkey());
    let ix = create_redelegation_intent_ix(payer.pubkey());
    let (sig, confirmed) = ctx
        .send_and_confirm_instructions_with_payer_ephem(&[ix], payer)
        .unwrap();
    assert!(confirmed);

    // Confirm was sent on Base Layer
    let commit_result = ctx
        .fetch_schedule_commit_result::<FlexiCounter>(sig)
        .unwrap();
    commit_result
        .confirm_commit_transactions_on_chain(ctx)
        .unwrap();

    // Confirm that it got delegated back
    let owner = ctx.fetch_chain_account_owner(pda).unwrap();
    assert_eq!(owner, dlp::id());
}
