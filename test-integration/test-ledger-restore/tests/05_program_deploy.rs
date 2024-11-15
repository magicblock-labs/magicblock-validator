use integration_test_tools::expect;
use integration_test_tools::tmpdir::resolve_tmp_dir;
use integration_test_tools::workspace_paths::TestProgramPaths;
use program_flexi_counter::instruction::create_add_ix;
use program_flexi_counter::instruction::create_init_ix;
use program_flexi_counter::instruction::create_mul_ix;
use program_flexi_counter::state::FlexiCounter;
use solana_sdk::native_token::LAMPORTS_PER_SOL;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Keypair;
use solana_sdk::signer::EncodableKey;
use solana_sdk::signer::Signer;
use std::io::{self, Write};
use std::path::Path;
use std::process;
use std::process::Child;
use test_ledger_restore::confirm_tx_with_payer;
use test_ledger_restore::fetch_counter;
use test_ledger_restore::{
    setup_offline_validator, FLEXI_COUNTER_ID, TMP_DIR_LEDGER,
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
    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);
    let payer = payer_keypair();
    let flexi_counter_paths = TestProgramPaths::new(
        "program_flexi_counter",
        "flexi-counter",
        FLEXI_COUNTER_ID,
    );

    let (mut validator, slot) =
        write(&ledger_path, &payer, &flexi_counter_paths);
    validator.kill().unwrap();

    let mut validator = read(&ledger_path, &payer.pubkey(), slot);
    validator.kill().unwrap();
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
    {
        let ix_init = create_init_ix(payer.pubkey(), COUNTER.to_string());
        confirm_tx_with_payer(ix_init, payer, &mut validator);

        let ix_add = create_add_ix(payer.pubkey(), 5);
        confirm_tx_with_payer(ix_add, payer, &mut validator);

        let ix_mul = create_mul_ix(payer.pubkey(), 2);
        confirm_tx_with_payer(ix_mul, payer, &mut validator);

        let counter = fetch_counter(&payer.pubkey(), &mut validator);
        assert_eq!(
            counter,
            FlexiCounter {
                count: 10,
                updates: 2,
                label: COUNTER.to_string()
            }
        )
    }

    let slot = expect!(ctx.wait_for_next_slot_ephem(), validator);
    (validator, slot)
}

fn read(ledger_path: &Path, payer: &Pubkey, slot: u64) -> Child {
    let (_, mut validator, ctx) =
        setup_offline_validator(ledger_path, None, None, false);

    assert!(ctx.wait_for_slot_ephem(slot).is_ok());

    let counter_decoded = fetch_counter(payer, &mut validator);
    assert_eq!(
        counter_decoded,
        FlexiCounter {
            count: 10,
            updates: 2,
            label: COUNTER.to_string()
        }
    );

    validator
}
