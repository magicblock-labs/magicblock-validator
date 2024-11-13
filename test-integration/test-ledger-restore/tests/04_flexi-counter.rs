use integration_test_tools::expect;
use integration_test_tools::tmpdir::resolve_tmp_dir;
use integration_test_tools::IntegrationTestContext;
use program_flexi_counter::instruction::create_init_ix;
use program_flexi_counter::state::FlexiCounter;
use sleipnir_config::ProgramConfig;
use solana_sdk::native_token::LAMPORTS_PER_SOL;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Keypair;
use solana_sdk::signature::Signature;
use solana_sdk::signer::Signer;
use solana_sdk::transaction::Transaction;
use std::path::Path;
use std::process::Child;
use test_ledger_restore::{
    setup_offline_validator, FLEXI_COUNTER_ID, SLOT_WRITE_DELTA, TMP_DIR_LEDGER,
};

const SLOT_MS: u64 = 150;

fn payer_keypair() -> Keypair {
    Keypair::from_base58_string("M8CcAuQHVQj91sKW68prBjNzvhEVjTj1ADMDej4KJTuwF4ckmibCmX3U6XGTMfGX5g7Xd43EXSNcjPkUWWcJpWA")
}
fn counter_keypair() -> Keypair {
    Keypair::from_base58_string("j5cwGmb19aNqc1Mc1n2xUSvZkG6vxjsYPHhLJC6RYmQbS1ggWeEU57jCnh5QwbrTzaCnDLE4UaS2wTVBWYyq5KT")
}

#[test]
fn restore_ledger_with_flexi_counter() {
    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);
    let payer = payer_keypair();

    let (mut validator, _sig, slot) = write(&ledger_path, &payer);
    // validator.kill().unwrap();

    // let mut validator =
    //     read(&ledger_path, &payer.pubkey(), &counter.pubkey(), slot);
    // validator.kill().unwrap();
}

fn get_programs() -> Vec<ProgramConfig> {
    vec![ProgramConfig {
        id: FLEXI_COUNTER_ID.try_into().unwrap(),
        path: "program_flexi_counter.so".to_string(),
    }]
}

fn write(ledger_path: &Path, payer: &Keypair) -> (Child, Signature, u64) {
    let programs = get_programs();
    // Choosing slower slots in order to have the airdrop + transaction occur in the
    // same slot and ensure that they are replayed in the correct order
    let (_, mut validator, ctx) = setup_offline_validator(
        ledger_path,
        Some(programs),
        Some(SLOT_MS),
        true,
    );

    expect!(ctx.wait_for_slot_ephem(1), validator);

    // 1. Airdrop to payer
    expect!(
        ctx.airdrop_ephem(&payer.pubkey(), LAMPORTS_PER_SOL),
        validator
    );

    // 2. Create and send init counter instruction
    let ix = create_init_ix(payer.pubkey(), "Counter 1".to_string());
    let mut tx = Transaction::new_with_payer(&[ix], Some(&payer.pubkey()));
    let signers = &[payer];

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
    payer: &Pubkey,
    counter: &Pubkey,
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

    let payer_acc = expect!(ctx.ephem_client.get_account(payer), validator);
    let counter_acc = expect!(ctx.ephem_client.get_account(counter), validator);

    eprintln!("payer: {:#?}", payer_acc);
    eprintln!("counter: {:#?}", counter_acc);

    validator
}

// -----------------
// Diagnose
// -----------------
// Uncomment either of the below to run ledger write/read in isolation and
// optionally keep the validator running after reading the ledger
#[test]
fn _flexi_counter_diagnose_write() {
    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);

    let payer = payer_keypair();
    eprintln!("{}", payer.to_base58_string());

    let (mut validator, sig, slot) = write(&ledger_path, &payer);

    let (counter, _) = FlexiCounter::pda(&payer.pubkey());
    eprintln!("{}", ledger_path.display());
    eprintln!("{} -> {}", payer.pubkey(), counter);
    eprintln!("{:?}", sig);
    eprintln!("slot: {}", slot);

    let ctx = IntegrationTestContext::new_ephem_only();
    let counter_acc =
        expect!(ctx.ephem_client.get_account(&counter), validator);
    let counter_decoded = FlexiCounter::try_decode(&counter_acc.data).unwrap();
    eprint!("{:#?}", counter_decoded);

    validator.kill().unwrap();
}

#[test]
fn _solx_single_diagnose_read() {
    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);

    let payer = payer_keypair();
    let counter = counter_keypair();

    let mut validator =
        read(&ledger_path, &payer.pubkey(), &counter.pubkey(), 20);

    eprintln!("{}", ledger_path.display());
    eprintln!("{} -> {}", payer.pubkey(), counter.pubkey());

    validator.kill().unwrap();
}
