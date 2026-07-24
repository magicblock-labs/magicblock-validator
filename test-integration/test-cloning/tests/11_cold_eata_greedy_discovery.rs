use std::{thread::sleep, time::Duration};

use dlp_api::pda::{
    delegate_buffer_pda_from_delegated_account_and_owner_program,
    delegation_metadata_pda_from_delegated_account,
    delegation_record_pda_from_delegated_account,
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

const SOURCE_EATA_BALANCE: u64 = 200;
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

/// A freshly delegated eATA of a never-seen wallet must be greedily cloned
/// and projected from the DLP program subscription alone. No account is read
/// on ephem before greedy discovery is observed: account reads trigger
/// on-demand cloning and would mask a broken firehose path, which is why the
/// probe polls `getSignaturesForAddress` (a pure ledger query).
#[test]
fn test_cold_eata_delegation_is_greedily_cloned_without_reads() {
    init_logger!();
    let ctx = IntegrationTestContext::try_new().unwrap();

    let fee_payer = Keypair::new();
    let source_authority = Keypair::new();
    let mint = Keypair::new();
    let source_ata = derive_ata(&source_authority.pubkey(), &mint.pubkey());
    let source_eata = derive_eata(&source_authority.pubkey(), &mint.pubkey());
    let validator = Keypair::try_from(&DLP_TEST_AUTHORITY_BYTES[..])
        .unwrap()
        .pubkey();

    ctx.airdrop_chain(&fee_payer.pubkey(), 2_000_000_000)
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
        spl_token_ix::mint_to(
            &spl_token::id(),
            &mint.pubkey(),
            &source_ata,
            &source_authority.pubkey(),
            &[],
            SOURCE_EATA_BALANCE,
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
    ];
    let mut eata_setup_tx =
        Transaction::new_with_payer(&eata_setup_ixs, Some(&fee_payer.pubkey()));
    let (_sig, confirmed) = ctx
        .send_and_confirm_transaction_chain(&mut eata_setup_tx, &[&fee_payer])
        .unwrap();
    assert!(confirmed, "eATA setup transaction failed");

    let deposit_ixs = vec![deposit_spl_tokens_ix(
        source_authority.pubkey(),
        source_authority.pubkey(),
        mint.pubkey(),
        SOURCE_EATA_BALANCE,
    )];
    let mut deposit_tx =
        Transaction::new_with_payer(&deposit_ixs, Some(&fee_payer.pubkey()));
    let (_sig, confirmed) = ctx
        .send_and_confirm_transaction_chain(
            &mut deposit_tx,
            &[&fee_payer, &source_authority],
        )
        .unwrap();
    assert!(confirmed, "eATA deposit transaction failed");

    let delegate_eata_ixs = vec![delegate_eata_ix(
        fee_payer.pubkey(),
        source_authority.pubkey(),
        mint.pubkey(),
        validator,
    )];
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

    let ephem_client = ctx.try_ephem_client().unwrap();
    let mut greedily_cloned = false;
    for _ in 0..100 {
        let ata_sigs = ephem_client
            .get_signatures_for_address(&source_ata)
            .unwrap_or_default();
        let eata_sigs = ephem_client
            .get_signatures_for_address(&source_eata)
            .unwrap_or_default();
        if !ata_sigs.is_empty() && !eata_sigs.is_empty() {
            greedily_cloned = true;
            break;
        }
        sleep(Duration::from_millis(200));
    }
    assert!(
        greedily_cloned,
        "freshly delegated eATA was not greedily cloned from the DLP program subscription"
    );

    assert_eq!(token_balance_chain(&ctx, &source_ata), 0);
    assert_eq!(
        token_balance_ephem(&ctx, &source_ata),
        Some(SOURCE_EATA_BALANCE)
    );
}
