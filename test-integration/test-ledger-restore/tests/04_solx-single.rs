use integration_test_tools::expect;
use integration_test_tools::tmpdir::resolve_tmp_dir;
use sleipnir_config::ProgramConfig;
use solana_sdk::instruction::{AccountMeta, Instruction};
use solana_sdk::native_token::LAMPORTS_PER_SOL;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Keypair;
use solana_sdk::signature::Signature;
use solana_sdk::signer::{SeedDerivable, Signer};
use solana_sdk::system_program;
use solana_sdk::transaction::Transaction;
use std::path::Path;
use std::process::Child;
use test_ledger_restore::{
    setup_offline_validator, SLOT_WRITE_DELTA, SOLX_ID, SOLX_PUBKEY,
    TMP_DIR_LEDGER,
};

const SLOT_MS: u64 = 150;

fn sender_keypair() -> Keypair {
    Keypair::from_base58_string("M8CcAuQHVQj91sKW68prBjNzvhEVjTj1ADMDej4KJTuwF4ckmibCmX3U6XGTMfGX5g7Xd43EXSNcjPkUWWcJpWA")
}
fn post_keypair() -> Keypair {
    Keypair::from_base58_string("j5cwGmb19aNqc1Mc1n2xUSvZkG6vxjsYPHhLJC6RYmQbS1ggWeEU57jCnh5QwbrTzaCnDLE4UaS2wTVBWYyq5KT")
}

#[test]
fn restore_ledger_with_solx_post() {
    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);
    let sender = sender_keypair();
    let post = post_keypair();

    let (mut validator, sig, slot) = write(&ledger_path, &sender, &post);
    validator.kill().unwrap();

    let mut validator =
        read(&ledger_path, &sender.pubkey(), &post.pubkey(), slot);
    validator.kill().unwrap();
}

fn get_programs() -> Vec<ProgramConfig> {
    vec![ProgramConfig {
        id: SOLX_ID.try_into().unwrap(),
        path: "fixtures/elfs/solanax.so".to_string(),
    }]
}

fn write(
    ledger_path: &Path,
    sender: &Keypair,
    post: &Keypair,
) -> (Child, Signature, u64) {
    let programs = get_programs();
    // Choosing slower slots in order to have the airdrop + transaction occur in the
    // same slot and ensure that they are replayed in the correct order
    let (_, mut validator, ctx) = setup_offline_validator(
        ledger_path,
        Some(programs),
        Some(SLOT_MS),
        true,
    );

    // 1. Airdrop to sender
    expect!(
        ctx.airdrop_ephem(&sender.pubkey(), LAMPORTS_PER_SOL),
        validator
    );

    // 2. Create and send post transaction
    let send_post_ix_args = [
        0x84, 0xf5, 0xee, 0x1d, 0xf3, 0x2a, 0xad, 0x36, 0x0b, 0x00, 0x00, 0x00,
        0x68, 0x65, 0x6c, 0x6c, 0x6f, 0x20, 0x77, 0x6f, 0x72, 0x6c, 0x64,
    ];
    let ix = Instruction::new_with_bytes(
        SOLX_PUBKEY,
        &send_post_ix_args,
        vec![
            AccountMeta::new(post.pubkey(), true),
            AccountMeta::new(sender.pubkey(), true),
            AccountMeta::new_readonly(system_program::id(), false),
        ],
    );
    let mut tx = Transaction::new_with_payer(&[ix], Some(&sender.pubkey()));
    let signers = &[post, sender];
    let (sig, confirmed) = expect!(
        ctx.send_and_confirm_transaction_ephem(&mut tx, signers),
        validator
    );

    assert!(confirmed, "Should confirm transaction");

    let slot = ctx.wait_for_delta_slot_ephem(SLOT_WRITE_DELTA).unwrap();

    (validator, sig, slot)
}

fn read(
    ledger_path: &Path,
    sender: &Pubkey,
    post: &Pubkey,
    slot: u64,
) -> Child {
    let programs = get_programs();
    let (_, mut validator, ctx) = setup_offline_validator(
        ledger_path,
        Some(programs),
        Some(SLOT_MS),
        false,
    );

    assert!(ctx.wait_for_slot_ephem(slot).is_ok());

    let sender_acc = expect!(ctx.ephem_client.get_account(sender), validator);
    let post_acc = expect!(ctx.ephem_client.get_account(post), validator);

    eprintln!("Sender: {:#?}", sender_acc);
    eprintln!("Post: {:#?}", post_acc);

    validator
}

// -----------------
// Diagnose
// -----------------
// Uncomment either of the below to run ledger write/read in isolation and
// optionally keep the validator running after reading the ledger
// 9r7Z9mZFP5GqVqjPg4z2s48Cxhr6tLtXMFfcqfAm5wSv -> 59SrSAKitdXcryYwPZB7FdPkoVH2XsY9SHgRTtYCpvcu
// U9zkLd8g7MdPH7ToRCb1eyczjuktcvvBKMbujJrvBUXYnpSFTbuZCbePWrP4659ygWi3zwn7YMc6UUSo2VWM8Fo
#[test]
fn _solx_single_diagnose_write() {
    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);

    let sender = sender_keypair();
    let post = post_keypair();
    eprintln!("{}", sender.to_base58_string());
    eprintln!("{}", post.to_base58_string());

    let (mut validator, sig, slot) = write(&ledger_path, &sender, &post);

    eprintln!("{}", ledger_path.display());
    eprintln!("{} -> {}", sender.pubkey(), post.pubkey());
    eprintln!("{:?}", sig);
    eprintln!("slot: {}", slot);

    validator.kill().unwrap();
}

#[test]
fn _solx_single_diagnose_read() {
    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);

    let sender = sender_keypair();
    let post = post_keypair();

    let mut validator =
        read(&ledger_path, &sender.pubkey(), &post.pubkey(), 20);

    eprintln!("{}", ledger_path.display());
    eprintln!("{} -> {}", sender.pubkey(), post.pubkey());

    // validator.kill().unwrap();
}
