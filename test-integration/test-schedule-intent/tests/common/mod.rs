use dlp_api::pda::ephemeral_balance_pda_from_payer;
use integration_test_tools::IntegrationTestContext;
use program_flexi_counter::{
    delegation_program_id,
    instruction::{create_add_ix, create_delegate_ix, create_init_ix},
    state::FlexiCounter,
};
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, pubkey::Pubkey, rent::Rent,
    signature::Keypair, signer::Signer,
};

pub const LABEL: &str = "I am a label";
pub const BASE_ACTION_FEE: u64 = 5000;
pub const CALLBACK_FEE: u64 = 5000;

pub fn setup_payer(ctx: &IntegrationTestContext) -> Keypair {
    let payer = Keypair::new();
    ctx.airdrop_chain(&payer.pubkey(), LAMPORTS_PER_SOL)
        .unwrap();

    let ix = dlp_api::instruction_builder::top_up_ephemeral_balance(
        payer.pubkey(),
        payer.pubkey(),
        Some(LAMPORTS_PER_SOL / 2),
        Some(1),
    );
    ctx.send_and_confirm_instructions_with_payer_chain(&[ix], &payer)
        .unwrap();

    let escrow_pda = ephemeral_balance_pda_from_payer(&payer.pubkey(), 1);
    let rent = Rent::default().minimum_balance(0);
    assert_eq!(
        ctx.fetch_chain_account(escrow_pda).unwrap().lamports,
        LAMPORTS_PER_SOL / 2 + rent
    );

    payer
}

pub fn init_counter(ctx: &IntegrationTestContext, payer: &Keypair) {
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

pub fn delegate_counter(ctx: &IntegrationTestContext, payer: &Keypair) {
    ctx.wait_for_next_slot_ephem().unwrap();

    let counter_pda = FlexiCounter::pda(&payer.pubkey()).0;
    let ix = create_delegate_ix(payer.pubkey());
    ctx.send_and_confirm_instructions_with_payer_chain(&[ix], payer)
        .unwrap();

    let owner = ctx.fetch_chain_account_owner(counter_pda).unwrap();
    assert_eq!(owner, delegation_program_id());
}

pub fn add_to_counter(
    ctx: &IntegrationTestContext,
    payer: &Keypair,
    value: u8,
) {
    ctx.wait_for_next_slot_ephem().unwrap();

    let counter_pda = FlexiCounter::pda(&payer.pubkey()).0;
    let counter_before = ctx
        .fetch_ephem_account_struct::<FlexiCounter>(counter_pda)
        .unwrap_or(FlexiCounter {
            count: 0,
            updates: 0,
            label: LABEL.to_string(),
        });

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

#[allow(unused)]
pub struct ExpectedCounter {
    pub pda: Pubkey,
    pub expected: u64,
}

#[allow(unused)]
pub fn assert_counters(
    ctx: &IntegrationTestContext,
    expected_counters: &[ExpectedCounter],
    is_base: bool,
) {
    let actual = expected_counters
        .iter()
        .map(|c| {
            if is_base {
                ctx.fetch_chain_account_struct::<FlexiCounter>(c.pda)
                    .unwrap()
            } else {
                ctx.fetch_ephem_account_struct::<FlexiCounter>(c.pda)
                    .unwrap()
            }
        })
        .collect::<Vec<_>>();

    for (i, actual_counter) in actual.iter().enumerate() {
        assert_eq!(actual_counter.count, expected_counters[i].expected);
    }
}

#[allow(unused)]
pub fn verify_undelegation_in_ephem_via_owner(
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
