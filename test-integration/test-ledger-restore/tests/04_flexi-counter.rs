use integration_test_tools::expect;
use integration_test_tools::tmpdir::resolve_tmp_dir;
use integration_test_tools::IntegrationTestContext;
use program_flexi_counter::instruction::create_init_ix;
use program_flexi_counter::state::FlexiCounter;
use sleipnir_config::ProgramConfig;
use solana_sdk::instruction::Instruction;
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

fn payer1_keypair() -> Keypair {
    Keypair::from_base58_string("M8CcAuQHVQj91sKW68prBjNzvhEVjTj1ADMDej4KJTuwF4ckmibCmX3U6XGTMfGX5g7Xd43EXSNcjPkUWWcJpWA")
}
fn payer2_keypair() -> Keypair {
    Keypair::from_base58_string("j5cwGmb19aNqc1Mc1n2xUSvZkG6vxjsYPHhLJC6RYmQbS1ggWeEU57jCnh5QwbrTzaCnDLE4UaS2wTVBWYyq5KT")
}

#[test]
fn restore_ledger_with_flexi_counter_same_slot() {
    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);
    let payer1 = payer1_keypair();
    let payer2 = payer2_keypair();

    let (mut validator, slot) = write(&ledger_path, &payer1, &payer2, false);
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

fn confirm_counter_tx(
    ix: Instruction,
    payer: &Keypair,
    validator: &mut Child,
) -> Signature {
    let ctx = IntegrationTestContext::new_ephem_only();

    let mut tx = Transaction::new_with_payer(&[ix], Some(&payer.pubkey()));
    let signers = &[payer];

    let (sig, confirmed) = expect!(
        ctx.send_and_confirm_transaction_ephem(&mut tx, signers),
        validator
    );
    assert!(confirmed, "Should confirm transaction");
    sig
}

fn fetch_counter(payer: &Pubkey, validator: &mut Child) -> FlexiCounter {
    let ctx = IntegrationTestContext::new_ephem_only();
    let (counter, _) = FlexiCounter::pda(payer);
    let counter_acc =
        expect!(ctx.ephem_client.get_account(&counter), validator);
    expect!(FlexiCounter::try_decode(&counter_acc.data), validator)
}

fn write(
    ledger_path: &Path,
    payer1: &Keypair,
    payer2: &Keypair,
    separate_slot: bool,
) -> (Child, u64) {
    const COUNTER1: &str = "Counter of Payer 1";
    const COUNTER2: &str = "Counter of Payer 2";

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

    // Airdrop to payers
    expect!(
        ctx.airdrop_ephem(&payer1.pubkey(), LAMPORTS_PER_SOL),
        validator
    );
    if separate_slot {
        expect!(ctx.wait_for_next_slot_ephem(), validator);
    }
    expect!(
        ctx.airdrop_ephem(&payer2.pubkey(), LAMPORTS_PER_SOL),
        validator
    );

    {
        // Create and send init counter1 instruction
        if separate_slot {
            expect!(ctx.wait_for_next_slot_ephem(), validator);
        }

        let ix = create_init_ix(payer1.pubkey(), COUNTER1.to_string());
        confirm_counter_tx(ix, payer1, &mut validator);
        let counter = fetch_counter(&payer1.pubkey(), &mut validator);
        assert_eq!(
            counter,
            FlexiCounter {
                count: 0,
                updates: 0,
                label: COUNTER1.to_string()
            }
        )
    }

    // TODO: in between update counter 1

    {
        // Create and send init counter1 instruction
        if separate_slot {
            expect!(ctx.wait_for_next_slot_ephem(), validator);
        }

        let ix = create_init_ix(payer2.pubkey(), COUNTER2.to_string());
        confirm_counter_tx(ix, payer2, &mut validator);
        let counter = fetch_counter(&payer2.pubkey(), &mut validator);
        assert_eq!(
            counter,
            FlexiCounter {
                count: 0,
                updates: 0,
                label: COUNTER2.to_string()
            }
        )
    }

    let slot = ctx.wait_for_delta_slot_ephem(SLOT_WRITE_DELTA).unwrap();

    (validator, slot)
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

    let payer1 = payer1_keypair();
    let payer2 = payer2_keypair();

    let (mut validator, slot) = write(&ledger_path, &payer1, &payer2, true);

    let (counter1, _) = FlexiCounter::pda(&payer1.pubkey());
    let (counter2, _) = FlexiCounter::pda(&payer2.pubkey());
    eprintln!("{}", ledger_path.display());
    eprintln!("1: {} -> {}", payer1.pubkey(), counter1);
    eprintln!("2: {} -> {}", payer2.pubkey(), counter2);
    eprintln!("slot: {}", slot);

    let counter1_decoded = fetch_counter(&payer1.pubkey(), &mut validator);
    let counter2_decoded = fetch_counter(&payer2.pubkey(), &mut validator);

    eprint!("1: {:#?}", counter1_decoded);
    eprint!("2: {:#?}", counter2_decoded);

    validator.kill().unwrap();
}

#[test]
fn _solx_single_diagnose_read() {
    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);

    let payer = payer1_keypair();
    let counter = payer2_keypair();

    let mut validator =
        read(&ledger_path, &payer.pubkey(), &counter.pubkey(), 20);

    eprintln!("{}", ledger_path.display());
    eprintln!("{} -> {}", payer.pubkey(), counter.pubkey());

    validator.kill().unwrap();
}
