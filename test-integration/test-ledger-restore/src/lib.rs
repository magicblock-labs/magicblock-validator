use std::{fs, path::Path, process, process::Child};

use integration_test_tools::{
    expect,
    tmpdir::resolve_tmp_dir,
    validator::{
        resolve_workspace_dir, start_magic_block_validator_with_config,
        TestRunnerPaths,
    },
    workspace_paths::path_relative_to_workspace,
    IntegrationTestContext,
};
use program_flexi_counter::state::FlexiCounter;
use sleipnir_config::{
    AccountsConfig, LedgerConfig, LifecycleMode, ProgramConfig, SleipnirConfig,
    ValidatorConfig,
};
use solana_sdk::{
    clock::Slot,
    instruction::Instruction,
    pubkey,
    pubkey::Pubkey,
    signature::{Keypair, Signature},
    signer::Signer,
    transaction::Transaction,
};
use tempfile::TempDir;

pub const TMP_DIR_LEDGER: &str = "TMP_DIR_LEDGER";
pub const TMP_DIR_CONFIG: &str = "TMP_DIR_CONFIG";
/// The minimum of slots we should wait for before shutting down a validator that
/// was writing the ledger.
pub const SLOT_WRITE_DELTA: Slot = 15;

pub const FLEXI_COUNTER_ID: &str =
    "f1exzKGtdeVX3d6UXZ89cY7twiNJe9S5uq84RTA4Rq4";
pub const FLEXI_COUNTER_PUBKEY: Pubkey =
    pubkey!("f1exzKGtdeVX3d6UXZ89cY7twiNJe9S5uq84RTA4Rq4");

/// Stringifies the config and writes it to a temporary config file.
/// Then uses that config to start the validator.
pub fn start_validator_with_config(
    config: SleipnirConfig,
) -> (TempDir, Option<process::Child>) {
    let workspace_dir = resolve_workspace_dir();
    let (default_tmpdir, temp_dir) = resolve_tmp_dir(TMP_DIR_CONFIG);
    let release = std::env::var("RELEASE").is_ok();
    let config_path = temp_dir.join("config.toml");
    let config_toml = config.to_string();
    fs::write(&config_path, config_toml).unwrap();

    let root_dir = Path::new(&workspace_dir)
        .join("..")
        .canonicalize()
        .unwrap()
        .to_path_buf();
    let paths = TestRunnerPaths {
        config_path,
        root_dir,
        workspace_dir,
    };
    (
        default_tmpdir,
        start_magic_block_validator_with_config(&paths, "TEST", release),
    )
}

pub fn setup_offline_validator(
    ledger_path: &Path,
    programs: Option<Vec<ProgramConfig>>,
    millis_per_slot: Option<u64>,
    reset: bool,
) -> (TempDir, Child, IntegrationTestContext) {
    let accounts_config = AccountsConfig {
        lifecycle: LifecycleMode::Offline,
        ..Default::default()
    };

    let validator_config = millis_per_slot
        .map(|ms| ValidatorConfig {
            millis_per_slot: ms,
            ..Default::default()
        })
        .unwrap_or_default();

    let programs = programs.map(|programs| {
        let mut resolved_programs = vec![];
        for program in programs.iter() {
            let p = path_relative_to_workspace(&format!(
                "target/deploy/{}",
                &program.path
            ));
            resolved_programs.push(ProgramConfig {
                id: program.id,
                path: p,
            });
        }
        resolved_programs
    });

    let config = SleipnirConfig {
        ledger: LedgerConfig {
            reset,
            path: Some(ledger_path.display().to_string()),
        },
        accounts: accounts_config.clone(),
        programs: programs.unwrap_or_default(),
        validator: validator_config,
        ..Default::default()
    };
    let (default_tmpdir_config, Some(validator)) =
        start_validator_with_config(config)
    else {
        panic!("validator should set up correctly");
    };

    let ctx = IntegrationTestContext::new_ephem_only();
    (default_tmpdir_config, validator, ctx)
}

// -----------------
// Transactions and Account Updates
// -----------------
pub fn send_tx_with_payer(
    ix: Instruction,
    payer: &Keypair,
    validator: &mut Child,
) -> Signature {
    let ctx = IntegrationTestContext::new_ephem_only();

    let mut tx = Transaction::new_with_payer(&[ix], Some(&payer.pubkey()));
    let signers = &[payer];

    let sig = expect!(ctx.send_transaction_ephem(&mut tx, signers), validator);
    sig
}

pub fn confirm_tx_with_payer(
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

pub fn fetch_counter(payer: &Pubkey, validator: &mut Child) -> FlexiCounter {
    let ctx = IntegrationTestContext::new_ephem_only();
    let (counter, _) = FlexiCounter::pda(payer);
    let counter_acc =
        expect!(ctx.ephem_client.get_account(&counter), validator);
    expect!(FlexiCounter::try_decode(&counter_acc.data), validator)
}
