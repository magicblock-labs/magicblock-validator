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
    RENT_PENDING_ATA_CLOSE_AUTHORITY, TOKEN_PROGRAM_ID,
};
use magicblock_magic_program_api::{
    instruction::MagicBlockInstruction, ID as MAGIC_PROGRAM_ID,
};
use solana_commitment_config::CommitmentConfig;
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
const RECEIVE_AMOUNT: u64 = 60;

// Ephemeral SPL Token program instruction tags used on chain / in the ER.
const INITIALIZE_EPHEMERAL_ATA: u8 = 0;
const INITIALIZE_GLOBAL_VAULT: u8 = 1;
const DEPOSIT_SPL_TOKENS: u8 = 2;
const DELEGATE_EPHEMERAL_ATA: u8 = 4;
const INITIALIZE_RENT_PDA: u8 = 23;
const WITHDRAW_THROUGH_DELEGATED_SHUTTLE_WITH_MERGE: u8 = 26;
const ENSURE_RENT_PENDING_DESTINATION: u8 = 35;

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

fn ephem_account_exists(
    ctx: &IntegrationTestContext,
    account: &Pubkey,
) -> bool {
    ctx.try_ephem_client()
        .unwrap()
        .get_account_with_commitment(account, CommitmentConfig::confirmed())
        .unwrap()
        .value
        .is_some()
}

/// True when the ER account at `account` carries the rent-pending marker
/// (close_authority == rent sysvar).
fn ephem_account_is_rent_pending(
    ctx: &IntegrationTestContext,
    account: &Pubkey,
) -> bool {
    ctx.try_ephem_client()
        .unwrap()
        .get_account_with_commitment(account, CommitmentConfig::confirmed())
        .unwrap()
        .value
        .is_some_and(|account| {
            account.data.len() >= 165
                && account.data[129..133] == 1u32.to_le_bytes()
                && account.data[133..165]
                    == RENT_PENDING_ATA_CLOSE_AUTHORITY.to_bytes()
        })
}

fn derive_global_vault(mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[mint.as_ref()], &EATA_PROGRAM_ID).0
}

fn derive_rent_pda() -> Pubkey {
    Pubkey::find_program_address(&[b"rent"], &EATA_PROGRAM_ID).0
}

fn derive_shuttle_metadata(
    owner: &Pubkey,
    mint: &Pubkey,
    shuttle_id: u32,
) -> Pubkey {
    Pubkey::find_program_address(
        &[owner.as_ref(), mint.as_ref(), &shuttle_id.to_le_bytes()],
        &EATA_PROGRAM_ID,
    )
    .0
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

fn ensure_rent_pending_destination_ix(
    payer: Pubkey,
    destination_owner: Pubkey,
    mint: Pubkey,
) -> Instruction {
    let destination_ata = derive_ata(&destination_owner, &mint);
    Instruction {
        program_id: EATA_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new_readonly(payer, true),
            AccountMeta::new_readonly(destination_owner, false),
            AccountMeta::new(destination_ata, false),
            AccountMeta::new_readonly(mint, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
            AccountMeta::new_readonly(MAGIC_PROGRAM_ID, false),
        ],
        data: vec![ENSURE_RENT_PENDING_DESTINATION],
    }
}

fn close_rent_pending_ata_ix(owner: Pubkey, mint: Pubkey) -> Instruction {
    Instruction {
        program_id: MAGIC_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new_readonly(owner, true),
            AccountMeta::new(derive_ata(&owner, &mint), false),
        ],
        data: MagicBlockInstruction::CloseRentPendingAta
            .try_to_vec()
            .unwrap(),
    }
}

fn withdraw_through_delegated_shuttle_ix(
    payer: Pubkey,
    owner: Pubkey,
    mint: Pubkey,
    shuttle_id: u32,
    amount: u64,
    validator: Pubkey,
) -> Instruction {
    let rent_pda = derive_rent_pda();
    let shuttle_metadata = derive_shuttle_metadata(&owner, &mint, shuttle_id);
    let shuttle_eata = derive_eata(&shuttle_metadata, &mint);
    let shuttle_wallet_ata = derive_ata(&shuttle_metadata, &mint);
    let owner_token = derive_ata(&owner, &mint);
    let delegation_buffer =
        delegate_buffer_pda_from_delegated_account_and_owner_program(
            &shuttle_eata,
            &EATA_PROGRAM_ID,
        );
    let delegation_record =
        delegation_record_pda_from_delegated_account(&shuttle_eata);
    let delegation_metadata =
        delegation_metadata_pda_from_delegated_account(&shuttle_eata);

    let mut data = Vec::with_capacity(45);
    data.push(WITHDRAW_THROUGH_DELEGATED_SHUTTLE_WITH_MERGE);
    data.extend_from_slice(&shuttle_id.to_le_bytes());
    data.extend_from_slice(&amount.to_le_bytes());
    data.extend_from_slice(validator.as_ref());

    Instruction {
        program_id: EATA_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new_readonly(payer, true),
            AccountMeta::new(rent_pda, false),
            AccountMeta::new(shuttle_metadata, false),
            AccountMeta::new(shuttle_eata, false),
            AccountMeta::new(shuttle_wallet_ata, false),
            AccountMeta::new_readonly(owner, true),
            AccountMeta::new_readonly(EATA_PROGRAM_ID, false),
            AccountMeta::new(delegation_buffer, false),
            AccountMeta::new(delegation_record, false),
            AccountMeta::new(delegation_metadata, false),
            AccountMeta::new_readonly(dlp_api::id(), false),
            AccountMeta::new_readonly(ASSOCIATED_TOKEN_PROGRAM_ID, false),
            AccountMeta::new_readonly(system_program::id(), false),
            AccountMeta::new(owner_token, false),
            AccountMeta::new_readonly(mint, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
        ],
        data,
    }
}

/// Sets up a mint whose supply is deposited into the eSPL global vault and
/// delegated through the source authority's eATA, so the source's ER-projected
/// ATA holds `SOURCE_EATA_BALANCE`. Returns (fee_payer, source_authority, mint).
fn setup_delegated_source(
    ctx: &IntegrationTestContext,
) -> (Keypair, Keypair, Keypair) {
    let fee_payer = Keypair::new();
    let source_authority = Keypair::new();
    let mint = Keypair::new();
    let source_ata = derive_ata(&source_authority.pubkey(), &mint.pubkey());
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

    let eata_ixs = vec![
        initialize_global_vault_ix(fee_payer.pubkey(), mint.pubkey()),
        initialize_eata_ix(
            fee_payer.pubkey(),
            source_authority.pubkey(),
            mint.pubkey(),
        ),
        deposit_spl_tokens_ix(
            source_authority.pubkey(),
            source_authority.pubkey(),
            mint.pubkey(),
            SOURCE_EATA_BALANCE,
        ),
        delegate_eata_ix(
            fee_payer.pubkey(),
            source_authority.pubkey(),
            mint.pubkey(),
            validator,
        ),
    ];
    let mut eata_tx =
        Transaction::new_with_payer(&eata_ixs, Some(&fee_payer.pubkey()));
    let (_sig, confirmed) = ctx
        .send_and_confirm_transaction_chain(
            &mut eata_tx,
            &[&fee_payer, &source_authority],
        )
        .unwrap();
    assert!(confirmed, "eATA setup transaction failed");

    // Project the delegated source eATA into the ER.
    ctx.fetch_ephem_account(source_ata).unwrap();
    assert_eq!(
        token_balance_ephem(ctx, &source_ata),
        Some(SOURCE_EATA_BALANCE)
    );

    (fee_payer, source_authority, mint)
}

/// Sends tokens inside the ER to a wallet that has no ATA anywhere: the eSPL
/// ensure instruction materializes a rent-pending ATA and a plain SPL transfer
/// funds it in the same transaction.
fn receive_into_rent_pending_ata(
    ctx: &IntegrationTestContext,
    ephem_payer: &Keypair,
    source_authority: &Keypair,
    destination: &Pubkey,
    mint: &Pubkey,
    amount: u64,
) {
    let source_ata = derive_ata(&source_authority.pubkey(), mint);
    let destination_ata = derive_ata(destination, mint);

    let ixs = vec![
        ensure_rent_pending_destination_ix(
            ephem_payer.pubkey(),
            *destination,
            *mint,
        ),
        spl_token_ix::transfer(
            &spl_token::id(),
            &source_ata,
            &destination_ata,
            &source_authority.pubkey(),
            &[],
            amount,
        )
        .unwrap(),
    ];
    let mut tx = Transaction::new_with_payer(&ixs, Some(&ephem_payer.pubkey()));
    let (_sig, confirmed) = ctx
        .send_and_confirm_transaction_ephem(
            &mut tx,
            &[ephem_payer, source_authority],
        )
        .unwrap();
    assert!(confirmed, "rent-pending receive transaction failed");

    assert_eq!(token_balance_ephem(ctx, &destination_ata), Some(amount));
}

#[test]
fn test_rent_pending_ata_receive_drain_and_close() {
    init_logger!();
    let ctx = IntegrationTestContext::try_new().unwrap();

    let (_fee_payer, source_authority, mint) = setup_delegated_source(&ctx);
    let mint = mint.pubkey();
    let source_ata = derive_ata(&source_authority.pubkey(), &mint);

    let ephem_payer = Keypair::new();
    ctx.airdrop_chain_escrowed(&ephem_payer, 2_000_000_000)
        .unwrap();

    // Receive into a wallet without any ATA: rent-pending ATA is created
    // and funded inside the ER, nothing exists on chain.
    let destination = Keypair::new();
    let destination_ata = derive_ata(&destination.pubkey(), &mint);
    receive_into_rent_pending_ata(
        &ctx,
        &ephem_payer,
        &source_authority,
        &destination.pubkey(),
        &mint,
        RECEIVE_AMOUNT,
    );
    assert!(ctx
        .try_chain_client()
        .unwrap()
        .get_account_with_commitment(
            &destination_ata,
            CommitmentConfig::confirmed()
        )
        .unwrap()
        .value
        .is_none());
    assert_eq!(
        token_balance_ephem(&ctx, &source_ata),
        Some(SOURCE_EATA_BALANCE - RECEIVE_AMOUNT)
    );

    // A rent-pending ATA left empty at the end of its creation transaction
    // rolls the whole transaction back.
    let empty_destination = Keypair::new();
    let ix = ensure_rent_pending_destination_ix(
        ephem_payer.pubkey(),
        empty_destination.pubkey(),
        mint,
    );
    let mut tx =
        Transaction::new_with_payer(&[ix], Some(&ephem_payer.pubkey()));
    assert!(
        !ctx.send_and_confirm_transaction_ephem(&mut tx, &[&ephem_payer])
            .map(|(_, confirmed)| confirmed)
            .unwrap_or(false),
        "unfunded rent-pending ATA creation must fail"
    );
    assert!(!ephem_account_exists(
        &ctx,
        &derive_ata(&empty_destination.pubkey(), &mint)
    ));

    // Spend the whole balance back, then explicitly close the drained
    // rent-pending ATA through the Magic Program.
    let ixs = vec![
        spl_token_ix::transfer(
            &spl_token::id(),
            &destination_ata,
            &source_ata,
            &destination.pubkey(),
            &[],
            RECEIVE_AMOUNT,
        )
        .unwrap(),
        close_rent_pending_ata_ix(destination.pubkey(), mint),
    ];
    let mut tx = Transaction::new_with_payer(&ixs, Some(&ephem_payer.pubkey()));
    let (_sig, confirmed) = ctx
        .send_and_confirm_transaction_ephem(
            &mut tx,
            &[&ephem_payer, &destination],
        )
        .unwrap();
    assert!(confirmed, "drain + close transaction failed");

    assert!(
        !ephem_account_exists(&ctx, &destination_ata),
        "closed rent-pending ATA must be removed from the ER"
    );
    assert_eq!(
        token_balance_ephem(&ctx, &source_ata),
        Some(SOURCE_EATA_BALANCE)
    );
}

#[test]
fn test_rent_pending_ata_full_withdrawal() {
    init_logger!();
    let ctx = IntegrationTestContext::try_new().unwrap();

    let (fee_payer, source_authority, mint) = setup_delegated_source(&ctx);
    let mint = mint.pubkey();
    let validator = Keypair::try_from(&DLP_TEST_AUTHORITY_BYTES[..])
        .unwrap()
        .pubkey();

    let ephem_payer = Keypair::new();
    ctx.airdrop_chain_escrowed(&ephem_payer, 2_000_000_000)
        .unwrap();

    let destination = Keypair::new();
    receive_into_rent_pending_ata(
        &ctx,
        &ephem_payer,
        &source_authority,
        &destination.pubkey(),
        &mint,
        RECEIVE_AMOUNT,
    );

    // Base-layer withdrawal in the SDK's rent-pending shape: idempotent ATA
    // create (payer covers init) + ix 26 — no eATA init/delegate.
    fund_withdrawal_sponsors(&ctx, &fee_payer);
    let withdraw_ixs = vec![
        create_associated_token_account_idempotent(
            &fee_payer.pubkey(),
            &destination.pubkey(),
            &mint,
            &spl_token::id(),
        ),
        withdraw_through_delegated_shuttle_ix(
            fee_payer.pubkey(),
            destination.pubkey(),
            mint,
            7,
            RECEIVE_AMOUNT,
            validator,
        ),
    ];
    let mut withdraw_tx =
        Transaction::new_with_payer(&withdraw_ixs, Some(&fee_payer.pubkey()));
    let (_sig, confirmed) = ctx
        .send_and_confirm_transaction_chain(
            &mut withdraw_tx,
            &[&fee_payer, &destination],
        )
        .unwrap();
    assert!(confirmed, "withdrawal transaction failed");

    assert_withdrawal_settled(&ctx, &destination.pubkey(), &mint, 7);
}

/// The SDK's default (non-rent-pending-aware) withdrawal shape — ATA create +
/// eATA init/delegate + ix 26 in one transaction — must also work over a
/// rent-pending source: the freshly delegated 0-amount eATA cannot clobber the
/// funded balance (the projection is deferred until it is drained), so callers
/// never need to distinguish the two source kinds.
#[test]
fn test_rent_pending_ata_transparent_withdrawal() {
    init_logger!();
    let ctx = IntegrationTestContext::try_new().unwrap();

    let (fee_payer, source_authority, mint) = setup_delegated_source(&ctx);
    let mint = mint.pubkey();
    let validator = Keypair::try_from(&DLP_TEST_AUTHORITY_BYTES[..])
        .unwrap()
        .pubkey();

    let ephem_payer = Keypair::new();
    ctx.airdrop_chain_escrowed(&ephem_payer, 2_000_000_000)
        .unwrap();

    let destination = Keypair::new();
    receive_into_rent_pending_ata(
        &ctx,
        &ephem_payer,
        &source_authority,
        &destination.pubkey(),
        &mint,
        RECEIVE_AMOUNT,
    );

    fund_withdrawal_sponsors(&ctx, &fee_payer);
    let withdraw_ixs = vec![
        create_associated_token_account_idempotent(
            &fee_payer.pubkey(),
            &destination.pubkey(),
            &mint,
            &spl_token::id(),
        ),
        initialize_eata_ix(fee_payer.pubkey(), destination.pubkey(), mint),
        delegate_eata_ix(
            fee_payer.pubkey(),
            destination.pubkey(),
            mint,
            validator,
        ),
        withdraw_through_delegated_shuttle_ix(
            fee_payer.pubkey(),
            destination.pubkey(),
            mint,
            9,
            RECEIVE_AMOUNT,
            validator,
        ),
    ];
    let mut withdraw_tx =
        Transaction::new_with_payer(&withdraw_ixs, Some(&fee_payer.pubkey()));
    let (_sig, confirmed) = ctx
        .send_and_confirm_transaction_chain(
            &mut withdraw_tx,
            &[&fee_payer, &destination],
        )
        .unwrap();
    assert!(confirmed, "transparent withdrawal transaction failed");

    assert_withdrawal_settled(&ctx, &destination.pubkey(), &mint, 9);

    // The destination came out lazily materialized: its base eATA stays
    // delegated for future use.
    let destination_eata = ctx
        .try_chain_client()
        .unwrap()
        .get_account_with_commitment(
            &derive_eata(&destination.pubkey(), &mint),
            CommitmentConfig::confirmed(),
        )
        .unwrap()
        .value
        .expect("destination eATA must exist after transparent withdrawal");
    assert_eq!(destination_eata.owner, dlp_api::id());
}

/// Initializes + funds the espl rent PDA (it fronts the shuttle accounts'
/// rent, reimbursed at settlement) and the DLP escrow at index 255 used by
/// the post-undelegate settle action.
fn fund_withdrawal_sponsors(ctx: &IntegrationTestContext, fee_payer: &Keypair) {
    let rent_pda = derive_rent_pda();
    let init_rent_pda_needed = ctx
        .try_chain_client()
        .unwrap()
        .get_account_with_commitment(&rent_pda, CommitmentConfig::confirmed())
        .unwrap()
        .value
        .is_none();
    let mut setup_ixs = Vec::new();
    if init_rent_pda_needed {
        setup_ixs.push(Instruction {
            program_id: EATA_PROGRAM_ID,
            accounts: vec![
                AccountMeta::new(fee_payer.pubkey(), true),
                AccountMeta::new(rent_pda, false),
                AccountMeta::new_readonly(system_program::id(), false),
            ],
            data: vec![INITIALIZE_RENT_PDA],
        });
    }
    setup_ixs.push(system_instruction::transfer(
        &fee_payer.pubkey(),
        &rent_pda,
        100_000_000,
    ));
    setup_ixs.push(dlp_api::instruction_builder::top_up_ephemeral_balance(
        fee_payer.pubkey(),
        fee_payer.pubkey(),
        Some(10_000_000),
        Some(255),
    ));
    let mut setup_tx =
        Transaction::new_with_payer(&setup_ixs, Some(&fee_payer.pubkey()));
    let (_sig, confirmed) = ctx
        .send_and_confirm_transaction_chain(&mut setup_tx, &[fee_payer])
        .unwrap();
    assert!(confirmed, "withdrawal setup transaction failed");
}

/// Waits for the withdrawal pipeline (ER fill + close actions, base commit,
/// undelegate, settle) and asserts the end state: tokens in the owner's base
/// ATA, no rent-pending marker left in the ER, shuttle accounts closed.
fn assert_withdrawal_settled(
    ctx: &IntegrationTestContext,
    destination: &Pubkey,
    mint: &Pubkey,
    shuttle_id: u32,
) {
    let destination_ata = derive_ata(destination, mint);
    let shuttle_metadata =
        derive_shuttle_metadata(destination, mint, shuttle_id);
    let shuttle_wallet_ata = derive_ata(&shuttle_metadata, mint);

    let mut base_balance = 0;
    for _ in 0..150 {
        base_balance = ctx
            .try_chain_client()
            .unwrap()
            .get_token_account_balance(&destination_ata)
            .ok()
            .and_then(|balance| balance.amount.parse::<u64>().ok())
            .unwrap_or(0);
        if base_balance == RECEIVE_AMOUNT {
            break;
        }
        sleep(Duration::from_millis(200));
    }
    if base_balance != RECEIVE_AMOUNT {
        // Dump the ER-side action transactions to diagnose pipeline stalls.
        for (name, address) in [
            ("ephem dest_ata", destination_ata),
            ("ephem shuttle_wallet", shuttle_wallet_ata),
        ] {
            let sigs = ctx
                .try_ephem_client()
                .unwrap()
                .get_signatures_for_address(&address)
                .unwrap_or_default();
            for sig in sigs.iter().take(5) {
                eprintln!(
                    "{name} tx {} err={:?} memo={:?}",
                    sig.signature, sig.err, sig.memo
                );
                if let Ok(parsed_sig) = sig.signature.parse() {
                    if let Ok(tx) = ctx
                        .try_ephem_client()
                        .unwrap()
                        .get_transaction(
                        &parsed_sig,
                        solana_transaction_status::UiTransactionEncoding::Json,
                    ) {
                        eprintln!(
                            "  logs: {:#?}",
                            tx.transaction.meta.map(|m| m.log_messages)
                        );
                    }
                }
            }
        }
    }
    assert_eq!(
        base_balance, RECEIVE_AMOUNT,
        "withdrawn tokens must arrive in the owner's base ATA"
    );

    // The fully drained rent-pending ATA was closed by the withdrawal action.
    // The address may reappear as a plain clone of the freshly created base
    // ATA (or an eATA projection), so assert the rent-pending marker is gone
    // rather than absence.
    let mut closed = false;
    for _ in 0..50 {
        if !ephem_account_is_rent_pending(ctx, &destination_ata) {
            closed = true;
            break;
        }
        sleep(Duration::from_millis(200));
    }
    assert!(closed, "drained rent-pending ATA must be closed in the ER");

    // Shuttle accounts are settled and closed on base.
    let chain_client = ctx.try_chain_client().unwrap();
    for account in [
        shuttle_metadata,
        derive_eata(&shuttle_metadata, mint),
        shuttle_wallet_ata,
    ] {
        assert!(
            chain_client
                .get_account_with_commitment(
                    &account,
                    CommitmentConfig::confirmed()
                )
                .unwrap()
                .value
                .is_none(),
            "shuttle account {account} must be closed after settlement"
        );
    }
}
