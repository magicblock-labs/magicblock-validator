use solana_rpc_client::rpc_client::RpcClient;
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
    AccountsConfig, LedgerConfig, LifecycleMode, ProgramConfig, RemoteConfig,
    SleipnirConfig, ValidatorConfig,
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

fn resolve_programs(
    programs: Option<Vec<ProgramConfig>>,
) -> Vec<ProgramConfig> {
    programs
        .map(|programs| {
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
        })
        .unwrap_or_default()
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

    let programs = resolve_programs(programs);

    let config = SleipnirConfig {
        ledger: LedgerConfig {
            reset,
            path: Some(ledger_path.display().to_string()),
        },
        accounts: accounts_config.clone(),
        programs,
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

/// This function sets up a validator that connects to a local remote.
/// That local remote is expected to listen on port 7799.
/// The [IntegrationTestContext] is setup to connect to both the ephemeral validator
/// and the local remote.
pub fn setup_validator_with_local_remote(
    ledger_path: &Path,
    programs: Option<Vec<ProgramConfig>>,
    reset: bool,
) -> (TempDir, Child, IntegrationTestContext) {
    let accounts_config = AccountsConfig {
        lifecycle: LifecycleMode::Ephemeral,
        remote: RemoteConfig::Custom(
            IntegrationTestContext::url_chain().try_into().unwrap(),
        ),
        ..Default::default()
    };
    let programs = resolve_programs(programs);

    let config = SleipnirConfig {
        ledger: LedgerConfig {
            reset,
            path: Some(ledger_path.display().to_string()),
        },
        accounts: accounts_config.clone(),
        programs,
        ..Default::default()
    };

    let (default_tmpdir_config, Some(validator)) =
        start_validator_with_config(config)
    else {
        panic!("validator should set up correctly");
    };

    let ctx = IntegrationTestContext::new();
    (default_tmpdir_config, validator, ctx)
}

// -----------------
// Transactions and Account Updates
// -----------------
pub fn send_tx_with_payer_ephem(
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

pub fn send_tx_with_payer_chain(
    ix: Instruction,
    payer: &Keypair,
    validator: &mut Child,
) -> Signature {
    let ctx = IntegrationTestContext::new();
    let mut tx = Transaction::new_with_payer(&[ix], Some(&payer.pubkey()));
    let signers = &[payer];

    let sig = expect!(ctx.send_transaction_chain(&mut tx, signers), validator);
    sig
}

pub fn confirm_tx_with_payer_ephem(
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

pub fn confirm_tx_with_payer_chain(
    ix: Instruction,
    payer: &Keypair,
    validator: &mut Child,
) -> Signature {
    let ctx = IntegrationTestContext::new();

    let mut tx = Transaction::new_with_payer(&[ix], Some(&payer.pubkey()));
    let signers = &[payer];

    let (sig, confirmed) = expect!(
        ctx.send_and_confirm_transaction_chain(&mut tx, signers),
        validator
    );
    assert!(confirmed, "Should confirm transaction");
    sig
}

pub fn fetch_counter_ephem(
    payer: &Pubkey,
    validator: &mut Child,
) -> FlexiCounter {
    let ctx = IntegrationTestContext::new_ephem_only();
    fetch_counter(payer, &ctx.ephem_client, validator)
}

pub fn fetch_counter_chain(
    payer: &Pubkey,
    validator: &mut Child,
) -> FlexiCounter {
    let ctx = IntegrationTestContext::new();
    fetch_counter(payer, ctx.try_chain_client().unwrap(), validator)
}

fn fetch_counter(
    payer: &Pubkey,
    rpc_client: &RpcClient,
    validator: &mut Child,
) -> FlexiCounter {
    let (counter, _) = FlexiCounter::pda(payer);
    let counter_acc = expect!(rpc_client.get_account(&counter), validator);
    expect!(FlexiCounter::try_decode(&counter_acc.data), validator)
}

pub fn fetch_counter_owner_chain(
    payer: &Pubkey,
    validator: &mut Child,
) -> Pubkey {
    let ctx = IntegrationTestContext::new();
    let (counter, _) = FlexiCounter::pda(payer);
    expect!(ctx.fetch_chain_account_owner(counter), validator)
}

// -----------------
// Slot Advances
// -----------------
/// Waits for sufficient slot advances to guarantee that the ledger for
/// the current slot was persiste
pub fn wait_for_ledger_persist(validator: &mut Child) -> Slot {
    let ctx = IntegrationTestContext::new_ephem_only();

    // I noticed test flakiness if we just advance to next slot once
    // It seems then the ledger hasn't been fully written by the time
    // we kill the validator and the most recent transactions + account
    // updates are missing.
    // Therefore we ensure to advance 3 slots instead of just one
    expect!(ctx.wait_for_next_slot_ephem(), validator);
    expect!(ctx.wait_for_next_slot_ephem(), validator);
    let slot = expect!(ctx.wait_for_next_slot_ephem(), validator);
    slot
}

// -----------------
// Scheduled Commits
// -----------------
pub fn assert_counter_commits_on_chain(
    ctx: &IntegrationTestContext,
    validator: &mut Child,
    payer: &Pubkey,
    expected_count: usize,
) {
    // Wait long enough for scheduled commits to have been handled
    expect!(ctx.wait_for_next_slot_ephem(), validator);
    expect!(ctx.wait_for_next_slot_ephem(), validator);
    expect!(ctx.wait_for_next_slot_ephem(), validator);

    let (pda, _) = FlexiCounter::pda(payer);
    let stats =
        expect!(ctx.get_signaturestats_for_address_chain(&pda), validator);
    assert_eq!(stats.len(), expected_count);
}

// -----------------
// Configs
// -----------------

pub fn get_programs_with_flexi_counter() -> Vec<ProgramConfig> {
    vec![ProgramConfig {
        id: FLEXI_COUNTER_ID.try_into().unwrap(),
        path: "program_flexi_counter.so".to_string(),
    }]
}
