use std::{thread::sleep, time::Duration};

use dlp_api::{
    args::DelegateArgs,
    instruction_builder::{
        delegate_with_actions, Encryptable, PostDelegationInstruction,
    },
    pda::{
        delegate_buffer_pda_from_delegated_account_and_owner_program,
        delegation_metadata_pda_from_delegated_account,
        delegation_record_pda_from_delegated_account,
    },
};
use integration_test_tools::{
    loaded_accounts::DLP_TEST_AUTHORITY_BYTES, IntegrationTestContext,
};
use magicblock_core::token_programs::{
    derive_ata, derive_eata, ASSOCIATED_TOKEN_PROGRAM_ID, EATA_PROGRAM_ID,
    TOKEN_PROGRAM_ID,
};
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    program_pack::Pack,
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
    transaction::Transaction,
};
use solana_system_interface::{
    instruction as system_instruction, program as system_program,
};
use spl_associated_token_account_interface::instruction::create_associated_token_account_idempotent;
use spl_token::{instruction as spl_token_ix, state::Mint};
use test_kit::init_logger;

const SOURCE_AUTHORITY_SEED: [u8; 32] = [7; 32];
const SOURCE_EATA_BALANCE: u64 = 200;
const DESTINATION_EATA_BALANCE: u64 = 100;
const TRANSFER_AMOUNT: u64 = 100;
const INITIALIZE_EPHEMERAL_ATA: u8 = 0;
const INITIALIZE_GLOBAL_VAULT: u8 = 1;
const DEPOSIT_SPL_TOKENS: u8 = 2;
const DELEGATE_EPHEMERAL_ATA: u8 = 4;

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

fn derive_global_vault(mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[mint.as_ref()], &EATA_PROGRAM_ID).0
}

fn initialize_global_vault_ix(payer: Pubkey, mint: Pubkey) -> Instruction {
    let vault = derive_global_vault(&mint);
    let vault_ephemeral_ata = derive_eata(&vault, &mint);
    let vault_ata = derive_ata(&vault, &mint);

    Instruction {
        program_id: EATA_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(vault, false),
            AccountMeta::new(payer, true),
            AccountMeta::new_readonly(mint, false),
            AccountMeta::new(vault_ephemeral_ata, false),
            AccountMeta::new(vault_ata, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
            AccountMeta::new_readonly(ASSOCIATED_TOKEN_PROGRAM_ID, false),
            AccountMeta::new_readonly(system_program::id(), false),
        ],
        data: vec![INITIALIZE_GLOBAL_VAULT],
    }
}

fn initialize_eata_ix(
    payer: Pubkey,
    user: Pubkey,
    mint: Pubkey,
) -> Instruction {
    Instruction {
        program_id: EATA_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(derive_eata(&user, &mint), false),
            AccountMeta::new(payer, true),
            AccountMeta::new_readonly(user, false),
            AccountMeta::new_readonly(mint, false),
            AccountMeta::new_readonly(system_program::id(), false),
        ],
        data: vec![INITIALIZE_EPHEMERAL_ATA],
    }
}

fn deposit_spl_tokens_ix(
    authority: Pubkey,
    user: Pubkey,
    mint: Pubkey,
    amount: u64,
) -> Instruction {
    let eata = derive_eata(&user, &mint);
    let vault = derive_global_vault(&mint);
    let user_source_token_acc = derive_ata(&user, &mint);
    let vault_token_acc = derive_ata(&vault, &mint);
    let mut data = Vec::with_capacity(9);
    data.push(DEPOSIT_SPL_TOKENS);
    data.extend_from_slice(&amount.to_le_bytes());

    Instruction {
        program_id: EATA_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(eata, false),
            AccountMeta::new_readonly(vault, false),
            AccountMeta::new_readonly(mint, false),
            AccountMeta::new(user_source_token_acc, false),
            AccountMeta::new(vault_token_acc, false),
            AccountMeta::new_readonly(authority, true),
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
        ],
        data,
    }
}

fn delegate_eata_ix(
    payer: Pubkey,
    user: Pubkey,
    mint: Pubkey,
    validator: Pubkey,
) -> Instruction {
    let eata = derive_eata(&user, &mint);
    let delegation_buffer =
        delegate_buffer_pda_from_delegated_account_and_owner_program(
            &eata,
            &EATA_PROGRAM_ID,
        );
    let delegation_record = delegation_record_pda_from_delegated_account(&eata);
    let delegation_metadata =
        delegation_metadata_pda_from_delegated_account(&eata);
    let mut data = Vec::with_capacity(33);
    data.push(DELEGATE_EPHEMERAL_ATA);
    data.extend_from_slice(validator.as_ref());

    Instruction {
        program_id: EATA_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(payer, true),
            AccountMeta::new(eata, false),
            AccountMeta::new_readonly(EATA_PROGRAM_ID, false),
            AccountMeta::new(delegation_buffer, false),
            AccountMeta::new(delegation_record, false),
            AccountMeta::new(delegation_metadata, false),
            AccountMeta::new_readonly(dlp_api::id(), false),
            AccountMeta::new_readonly(system_program::id(), false),
        ],
        data,
    }
}

#[test]
fn test_post_delegation_action_executes_spl_token_transfer_100() {
    init_logger!();
    let ctx = IntegrationTestContext::try_new().unwrap();

    let fee_payer = Keypair::new();
    let delegated_account = Keypair::new();
    let source_authority = Keypair::new_from_array(SOURCE_AUTHORITY_SEED);
    let destination_authority = Keypair::new_from_array([8; 32]);
    let mint = Keypair::new_from_array([9; 32]);
    let source_ata = derive_ata(&source_authority.pubkey(), &mint.pubkey());
    let destination_ata =
        derive_ata(&destination_authority.pubkey(), &mint.pubkey());
    let validator = Keypair::try_from(&DLP_TEST_AUTHORITY_BYTES[..])
        .unwrap()
        .pubkey();

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
            &destination_authority.pubkey(),
            &mint.pubkey(),
            &spl_token::id(),
        ),
        spl_token_ix::mint_to(
            &spl_token::id(),
            &mint.pubkey(),
            &source_ata,
            &source_authority.pubkey(),
            &[],
            SOURCE_EATA_BALANCE,
        )
        .unwrap(),
        spl_token_ix::mint_to(
            &spl_token::id(),
            &mint.pubkey(),
            &destination_ata,
            &source_authority.pubkey(),
            &[],
            DESTINATION_EATA_BALANCE,
        )
        .unwrap(),
    ];
    let mut setup_tx =
        Transaction::new_with_payer(&setup_ixs, Some(&fee_payer.pubkey()));
    let (_sig, confirmed) = ctx
        .send_and_confirm_transaction_chain(
            &mut setup_tx,
            &[&fee_payer, &mint, &source_authority],
        )
        .unwrap();
    assert!(confirmed, "setup transaction failed");

    let eata_setup_ixs = vec![
        initialize_global_vault_ix(fee_payer.pubkey(), mint.pubkey()),
        initialize_eata_ix(
            fee_payer.pubkey(),
            source_authority.pubkey(),
            mint.pubkey(),
        ),
        initialize_eata_ix(
            fee_payer.pubkey(),
            destination_authority.pubkey(),
            mint.pubkey(),
        ),
    ];
    let mut eata_setup_tx =
        Transaction::new_with_payer(&eata_setup_ixs, Some(&fee_payer.pubkey()));
    let (_sig, confirmed) = ctx
        .send_and_confirm_transaction_chain(&mut eata_setup_tx, &[&fee_payer])
        .unwrap();
    assert!(confirmed, "eATA setup transaction failed");

    let deposit_ixs = vec![
        deposit_spl_tokens_ix(
            source_authority.pubkey(),
            source_authority.pubkey(),
            mint.pubkey(),
            SOURCE_EATA_BALANCE,
        ),
        deposit_spl_tokens_ix(
            destination_authority.pubkey(),
            destination_authority.pubkey(),
            mint.pubkey(),
            DESTINATION_EATA_BALANCE,
        ),
    ];
    let mut deposit_tx =
        Transaction::new_with_payer(&deposit_ixs, Some(&fee_payer.pubkey()));
    let (_sig, confirmed) = ctx
        .send_and_confirm_transaction_chain(
            &mut deposit_tx,
            &[&fee_payer, &source_authority, &destination_authority],
        )
        .unwrap();
    assert!(confirmed, "eATA deposit transaction failed");

    let delegate_eata_ixs = vec![
        delegate_eata_ix(
            fee_payer.pubkey(),
            source_authority.pubkey(),
            mint.pubkey(),
            validator,
        ),
        delegate_eata_ix(
            fee_payer.pubkey(),
            destination_authority.pubkey(),
            mint.pubkey(),
            validator,
        ),
    ];
    let mut delegate_eata_tx = Transaction::new_with_payer(
        &delegate_eata_ixs,
        Some(&fee_payer.pubkey()),
    );
    let (_sig, confirmed) = ctx
        .send_and_confirm_transaction_chain(
            &mut delegate_eata_tx,
            &[&fee_payer],
        )
        .unwrap();
    assert!(confirmed, "eATA delegation transaction failed");

    // The eATAs are loaded as delegated accounts, so fetching the ATAs projects
    // delegated eATA balances into ephem.
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
