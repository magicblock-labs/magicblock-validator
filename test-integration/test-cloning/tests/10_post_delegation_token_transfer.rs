use std::{thread::sleep, time::Duration};

use dlp_api::{
    dlp::args::DelegateArgs,
    instruction_builder::{
        delegate_with_actions, Encryptable, PostDelegationInstruction,
    },
};
use integration_test_tools::{
    loaded_accounts::DLP_TEST_AUTHORITY_BYTES, IntegrationTestContext,
};
use solana_sdk::{
    signature::Keypair, signer::Signer, system_instruction,
    transaction::Transaction,
};
use spl_token::{
    instruction as spl_token_ix,
    solana_program::program_pack::Pack,
    state::{Account as TokenAccount, Mint},
};
use test_kit::init_logger;

fn token_balance_chain(ctx: &IntegrationTestContext, account: &Keypair) -> u64 {
    let balance = ctx
        .try_chain_client()
        .unwrap()
        .get_token_account_balance(&account.pubkey())
        .unwrap();
    balance.amount.parse::<u64>().unwrap()
}

fn token_balance_ephem(
    ctx: &IntegrationTestContext,
    account: &Keypair,
) -> Option<u64> {
    ctx.try_ephem_client()
        .unwrap()
        .get_token_account_balance(&account.pubkey())
        .ok()
        .and_then(|balance| balance.amount.parse::<u64>().ok())
}

#[test]
fn test_post_delegation_action_executes_spl_token_transfer_100() {
    init_logger!();
    let ctx = IntegrationTestContext::try_new().unwrap();

    let fee_payer = Keypair::new();
    let delegated_account = Keypair::new();
    let token_authority = Keypair::new();
    let mint = Keypair::new();
    let token_x = Keypair::new();
    let token_y = Keypair::new();

    ctx.airdrop_chain(&fee_payer.pubkey(), 2_000_000_000)
        .unwrap();
    ctx.airdrop_chain(&delegated_account.pubkey(), 2_000_000_000)
        .unwrap();
    ctx.airdrop_chain(&token_authority.pubkey(), 2_000_000_000)
        .unwrap();

    let chain_client = ctx.try_chain_client().unwrap();
    let mint_rent = chain_client
        .get_minimum_balance_for_rent_exemption(Mint::LEN)
        .unwrap();
    let token_rent = chain_client
        .get_minimum_balance_for_rent_exemption(TokenAccount::LEN)
        .unwrap();

    let setup_ixs = vec![
        system_instruction::create_account(
            &token_authority.pubkey(),
            &mint.pubkey(),
            mint_rent,
            Mint::LEN as u64,
            &spl_token::id(),
        ),
        spl_token_ix::initialize_mint(
            &spl_token::id(),
            &mint.pubkey(),
            &token_authority.pubkey(),
            None,
            0,
        )
        .unwrap(),
        system_instruction::create_account(
            &token_authority.pubkey(),
            &token_x.pubkey(),
            token_rent,
            TokenAccount::LEN as u64,
            &spl_token::id(),
        ),
        spl_token_ix::initialize_account(
            &spl_token::id(),
            &token_x.pubkey(),
            &mint.pubkey(),
            &token_authority.pubkey(),
        )
        .unwrap(),
        system_instruction::create_account(
            &token_authority.pubkey(),
            &token_y.pubkey(),
            token_rent,
            TokenAccount::LEN as u64,
            &spl_token::id(),
        ),
        spl_token_ix::initialize_account(
            &spl_token::id(),
            &token_y.pubkey(),
            &mint.pubkey(),
            &token_authority.pubkey(),
        )
        .unwrap(),
        spl_token_ix::mint_to(
            &spl_token::id(),
            &mint.pubkey(),
            &token_x.pubkey(),
            &token_authority.pubkey(),
            &[],
            200,
        )
        .unwrap(),
        spl_token_ix::mint_to(
            &spl_token::id(),
            &mint.pubkey(),
            &token_y.pubkey(),
            &token_authority.pubkey(),
            &[],
            100,
        )
        .unwrap(),
    ];
    let mut setup_tx = Transaction::new_with_payer(
        &setup_ixs,
        Some(&token_authority.pubkey()),
    );
    let (_sig, confirmed) = ctx
        .send_and_confirm_transaction_chain(
            &mut setup_tx,
            &[&token_authority, &mint, &token_x, &token_y],
        )
        .unwrap();
    assert!(confirmed);

    // Clone token accounts once so we can assert pre/post balances in ephem.
    ctx.fetch_ephem_account(token_x.pubkey()).unwrap();
    ctx.fetch_ephem_account(token_y.pubkey()).unwrap();

    assert_eq!(token_balance_chain(&ctx, &token_x), 200);
    assert_eq!(token_balance_chain(&ctx, &token_y), 100);
    assert_eq!(token_balance_ephem(&ctx, &token_x), Some(200));
    assert_eq!(token_balance_ephem(&ctx, &token_y), Some(100));

    let transfer_100_ix = spl_token_ix::transfer(
        &spl_token::id(),
        &token_x.pubkey(),
        &token_y.pubkey(),
        &token_authority.pubkey(),
        &[],
        200,
    )
    .unwrap();
    let post_actions: Vec<PostDelegationInstruction> =
        vec![transfer_100_ix.cleartext()];

    let validator = Keypair::from_bytes(&DLP_TEST_AUTHORITY_BYTES)
        .unwrap()
        .pubkey();
    let delegate_with_actions_ix = delegate_with_actions(
        fee_payer.pubkey(),
        delegated_account.pubkey(),
        None,
        DelegateArgs {
            commit_frequency_ms: u32::MAX,
            seeds: vec![],
            validator: Some(validator),
        },
        post_actions,
    );

    let assign_ix = system_instruction::assign(
        &delegated_account.pubkey(),
        &dlp_api::dlp::id(),
    );
    let mut assign_tx =
        Transaction::new_with_payer(&[assign_ix], Some(&fee_payer.pubkey()));
    let (_sig, confirmed) = ctx
        .send_and_confirm_transaction_chain(
            &mut assign_tx,
            &[&fee_payer, &delegated_account],
        )
        .unwrap();
    assert!(confirmed, "assign transaction failed");

    let mut delegate_tx = Transaction::new_with_payer(
        &[delegate_with_actions_ix],
        Some(&fee_payer.pubkey()),
    );
    let (_sig, confirmed) = ctx
        .send_and_confirm_transaction_chain(
            &mut delegate_tx,
            &[&fee_payer, &delegated_account, &token_authority],
        )
        .unwrap();
    assert!(confirmed);

    // Trigger delegated account clone; this is where post-delegation actions
    // are executed on ephem.
    ctx.fetch_ephem_account(delegated_account.pubkey()).unwrap();

    // Poll briefly because clone + action execution is async relative to fetch.
    let mut found = None;
    for _ in 0..30 {
        let bx = token_balance_ephem(&ctx, &token_x);
        let by = token_balance_ephem(&ctx, &token_y);
        if bx == Some(0) && by == Some(100) {
            found = Some((bx, by));
            break;
        }
        sleep(Duration::from_millis(200));
    }
    let (x_after, y_after) = found.unwrap_or_else(|| {
        (
            token_balance_ephem(&ctx, &token_x),
            token_balance_ephem(&ctx, &token_y),
        )
    });

    assert_eq!(
        x_after,
        Some(0),
        "x_after: {:?}, y_after: {:?}",
        x_after,
        y_after
    );
    assert_eq!(
        y_after,
        Some(300),
        "x_after: {:?}, y_after: {:?}",
        x_after,
        y_after
    );
}
