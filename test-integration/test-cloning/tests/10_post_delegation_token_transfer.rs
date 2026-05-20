use std::{thread::sleep, time::Duration};

use dlp_api::{
    args::DelegateArgs,
    instruction_builder::{
        delegate_with_actions, Encryptable, PostDelegationInstruction,
    },
};
use integration_test_tools::{
    loaded_accounts::DLP_TEST_AUTHORITY_BYTES, IntegrationTestContext,
};
use magicblock_core::token_programs::derive_ata;
use solana_sdk::{
    program_pack::Pack,
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
    transaction::Transaction,
};
use solana_system_interface::instruction as system_instruction;
use spl_associated_token_account_interface::instruction::create_associated_token_account_idempotent;
use spl_token::{instruction as spl_token_ix, state::Mint};
use test_kit::init_logger;

const SOURCE_AUTHORITY_SEED: [u8; 32] = [7; 32];
const SOURCE_EATA_BALANCE: u64 = 200;
const DESTINATION_EATA_BALANCE: u64 = 100;
const TRANSFER_AMOUNT: u64 = 100;

fn token_balance_chain(ctx: &IntegrationTestContext, account: &Pubkey) -> u64 {
    let balance = ctx
        .try_chain_client()
        .unwrap()
        .get_token_account_balance(account)
        .unwrap();
    balance.amount.parse::<u64>().unwrap()
}

fn token_balance_ephem(
    ctx: &IntegrationTestContext,
    account: &Pubkey,
) -> Option<u64> {
    ctx.try_ephem_client()
        .unwrap()
        .get_token_account_balance(account)
        .ok()
        .and_then(|balance| balance.amount.parse::<u64>().ok())
}

#[test]
fn test_post_delegation_action_executes_spl_token_transfer_100() {
    init_logger!();
    let ctx = IntegrationTestContext::try_new().unwrap();

    let fee_payer = Keypair::new();
    let delegated_account = Keypair::new();
    let source_authority = Keypair::new_from_array(SOURCE_AUTHORITY_SEED);
    let destination_authority = Keypair::new_from_array([8; 32]).pubkey();
    let mint = Keypair::new_from_array([9; 32]);
    let source_ata = derive_ata(&source_authority.pubkey(), &mint.pubkey());
    let destination_ata = derive_ata(&destination_authority, &mint.pubkey());

    ctx.airdrop_chain(&fee_payer.pubkey(), 2_000_000_000)
        .unwrap();
    ctx.airdrop_chain(&delegated_account.pubkey(), 2_000_000_000)
        .unwrap();
    ctx.airdrop_chain(&source_authority.pubkey(), 2_000_000_000)
        .unwrap();

    let chain_client = ctx.try_chain_client().unwrap();
    let mint_rent = chain_client
        .get_minimum_balance_for_rent_exemption(Mint::LEN)
        .unwrap();

    let setup_ixs = vec![
        system_instruction::create_account(
            &fee_payer.pubkey(),
            &mint.pubkey(),
            mint_rent,
            Mint::LEN as u64,
            &spl_token::id(),
        ),
        spl_token_ix::initialize_mint(
            &spl_token::id(),
            &mint.pubkey(),
            &source_authority.pubkey(),
            None,
            0,
        )
        .unwrap(),
        create_associated_token_account_idempotent(
            &fee_payer.pubkey(),
            &source_authority.pubkey(),
            &mint.pubkey(),
            &spl_token::id(),
        ),
        create_associated_token_account_idempotent(
            &fee_payer.pubkey(),
            &destination_authority,
            &mint.pubkey(),
            &spl_token::id(),
        ),
    ];
    let mut setup_tx =
        Transaction::new_with_payer(&setup_ixs, Some(&fee_payer.pubkey()));
    let (_sig, confirmed) = ctx
        .send_and_confirm_transaction_chain(&mut setup_tx, &[&fee_payer, &mint])
        .unwrap();
    assert!(confirmed, "setup transaction failed");

    // The companion eATAs and their delegation records are loaded as delegated
    // fixtures, so fetching the ATAs projects delegated eATA balances into
    // ephem.
    ctx.fetch_ephem_account(source_ata).unwrap();
    ctx.fetch_ephem_account(destination_ata).unwrap();

    assert_eq!(token_balance_chain(&ctx, &source_ata), 0);
    assert_eq!(token_balance_chain(&ctx, &destination_ata), 0);
    assert_eq!(
        token_balance_ephem(&ctx, &source_ata),
        Some(SOURCE_EATA_BALANCE)
    );
    assert_eq!(
        token_balance_ephem(&ctx, &destination_ata),
        Some(DESTINATION_EATA_BALANCE)
    );

    let transfer_100_ix = spl_token_ix::transfer(
        &spl_token::id(),
        &source_ata,
        &destination_ata,
        &source_authority.pubkey(),
        &[],
        TRANSFER_AMOUNT,
    )
    .unwrap();
    let post_actions: Vec<PostDelegationInstruction> =
        vec![transfer_100_ix.cleartext()];

    let validator = Keypair::try_from(&DLP_TEST_AUTHORITY_BYTES[..])
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

    let assign_ix =
        system_instruction::assign(&delegated_account.pubkey(), &dlp_api::id());
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
            &[&fee_payer, &delegated_account, &source_authority],
        )
        .unwrap();
    assert!(confirmed);

    // Poll briefly because clone + action execution is async relative to post-delegation actions
    // auto-detection.
    let mut found = None;
    for _ in 0..60 {
        let bx = token_balance_ephem(&ctx, &source_ata);
        let by = token_balance_ephem(&ctx, &destination_ata);
        if bx == Some(SOURCE_EATA_BALANCE - TRANSFER_AMOUNT)
            && by == Some(DESTINATION_EATA_BALANCE + TRANSFER_AMOUNT)
        {
            found = Some((bx, by));
            break;
        }
        sleep(Duration::from_millis(200));
    }
    let (x_after, y_after) = found.unwrap_or_else(|| {
        (
            token_balance_ephem(&ctx, &source_ata),
            token_balance_ephem(&ctx, &destination_ata),
        )
    });

    assert_eq!(
        x_after,
        Some(SOURCE_EATA_BALANCE - TRANSFER_AMOUNT),
        "x_after: {:?}, y_after: {:?}",
        x_after,
        y_after
    );
    assert_eq!(
        y_after,
        Some(DESTINATION_EATA_BALANCE + TRANSFER_AMOUNT),
        "x_after: {:?}, y_after: {:?}",
        x_after,
        y_after
    );
}
