#![allow(unused)]

use cleanass::assert_eq;
use std::{
    io::{self, Write},
    path::Path,
    process,
    process::Child,
};

use integration_test_tools::{
    expect, tmpdir::resolve_tmp_dir, workspace_paths::TestProgramPaths,
};
use program_flexi_counter::{
    instruction::{create_add_ix, create_init_ix, create_mul_ix},
    state::FlexiCounter,
};
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL,
    pubkey::Pubkey,
    signature::Keypair,
    signer::{EncodableKey, Signer},
};
use test_ledger_restore::{
    cleanup, confirm_tx_with_payer_ephem, fetch_counter_ephem,
    setup_offline_validator, wait_for_ledger_persist, FLEXI_COUNTER_ID,
    TMP_DIR_LEDGER,
};

fn read_authority_pubkey(paths: &TestProgramPaths) -> Pubkey {
    let keypair =
        Keypair::read_from_file(&paths.authority_keypair_path).unwrap();
    keypair.pubkey()
}

fn payer_keypair() -> Keypair {
    Keypair::from_base58_string("M8CcAuQHVQj91sKW68prBjNzvhEVjTj1ADMDej4KJTuwF4ckmibCmX3U6XGTMfGX5g7Xd43EXSNcjPkUWWcJpWA")
}

const COUNTER: &str = "Counter of Payer";

#[test]
fn restore_ledger_with_flexi_counter_deploy() {
    //let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);
    //let payer = payer_keypair();
    //let flexi_counter_paths = TestProgramPaths::new(
    //    "program_flexi_counter",
    //    "flexi-counter",
    //    FLEXI_COUNTER_ID,
    //);
    //
    //let (mut validator, _) = write(&ledger_path, &payer, &flexi_counter_paths);
    //validator.kill().unwrap();
    //
    //let mut validator = read(&ledger_path, &payer.pubkey());
    //validator.kill().unwrap();
}

fn write(
    ledger_path: &Path,
    payer: &Keypair,
    flexi_counter_paths: &TestProgramPaths,
) -> (Child, u64) {
    let authority = read_authority_pubkey(flexi_counter_paths);

    let (_, mut validator, ctx) =
        setup_offline_validator(ledger_path, None, None, true);

    expect!(ctx.wait_for_slot_ephem(1), validator);

    expect!(
        ctx.airdrop_ephem(&authority, 5 * LAMPORTS_PER_SOL),
        validator
    );
    expect!(
        ctx.airdrop_ephem(&payer.pubkey(), LAMPORTS_PER_SOL),
        validator
    );

    // First we deploy using the `solana deploy` command which will result in
    // a lot of transactions.
    let deploy_cmd = &mut process::Command::new("solana");
    deploy_cmd
        .args(["program", "deploy"])
        .args(["-u", "localhost"])
        .args(["--keypair", &flexi_counter_paths.authority_keypair_path])
        .args(["--program-id", &flexi_counter_paths.program_keypair_path])
        .arg(&flexi_counter_paths.program_path);

    let output = expect!(deploy_cmd.output(), validator);
    io::stdout().write_all(&output.stdout).unwrap();
    io::stderr().write_all(&output.stderr).unwrap();
    eprintln!("Deploy status: {}", output.status);

    // Second we mainly test that the program was properly deployed by running
    // a few transactions
    expect!(ctx.wait_for_next_slot_ephem(), validator);

    let ix_init = create_init_ix(payer.pubkey(), COUNTER.to_string());
    confirm_tx_with_payer_ephem(ix_init, payer, &mut validator);

    let ix_add = create_add_ix(payer.pubkey(), 5);
    confirm_tx_with_payer_ephem(ix_add, payer, &mut validator);

    let ix_mul = create_mul_ix(payer.pubkey(), 2);
    confirm_tx_with_payer_ephem(ix_mul, payer, &mut validator);

    let counter = fetch_counter_ephem(&payer.pubkey(), &mut validator);
    assert_eq!(
        counter,
        FlexiCounter {
            count: 10,
            updates: 2,
            label: COUNTER.to_string()
        },
        cleanup(&mut validator)
    );

    let slot = wait_for_ledger_persist(&mut validator);
    (validator, slot)
}

fn read(ledger_path: &Path, payer: &Pubkey) -> Child {
    let (_, mut validator, _) =
        setup_offline_validator(ledger_path, None, None, false);

    let counter_decoded = fetch_counter_ephem(payer, &mut validator);
    assert_eq!(
        counter_decoded,
        FlexiCounter {
            count: 10,
            updates: 2,
            label: COUNTER.to_string()
        },
        cleanup(&mut validator)
    );

    validator
}
