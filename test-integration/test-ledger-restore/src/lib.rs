use solana_rpc_client::rpc_client::RpcClient;
use std::{
    fs,
    path::Path,
    process::{self, Child},
    thread::sleep,
    time::Duration,
};

use integration_test_tools::{
    expect,
    loaded_accounts::LoadedAccounts,
    tmpdir::resolve_tmp_dir,
    validator::{
        resolve_workspace_dir, start_magic_block_validator_with_config,
        TestRunnerPaths,
    },
    workspace_paths::path_relative_to_workspace,
    IntegrationTestContext,
};
use magicblock_config::{
    AccountsConfig, EphemeralConfig, LedgerConfig, LifecycleMode,
    ProgramConfig, RemoteCluster, RemoteConfig, ValidatorConfig,
    DEFAULT_LEDGER_SIZE_BYTES,
};
use program_flexi_counter::state::FlexiCounter;
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
    config: EphemeralConfig,
    loaded_chain_accounts: &LoadedAccounts,
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
        start_magic_block_validator_with_config(
            &paths,
            "TEST",
            loaded_chain_accounts,
            release,
        ),
    )
}

fn resolve_programs(
    programs: Option<Vec<ProgramConfig>>,
) -> Vec<ProgramConfig> {
    programs
        .map(|programs| {
            programs
                .into_iter()
                .map(|program| ProgramConfig {
                    id: program.id,
                    path: path_relative_to_workspace(&format!(
                        "target/deploy/{}",
                        program.path
                    )),
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn setup_offline_validator(
    ledger_path: &Path,
    programs: Option<Vec<ProgramConfig>>,
    millis_per_slot: Option<u64>,
    reset: bool,
) -> (TempDir, Child, IntegrationTestContext) {
    let mut accounts_config = AccountsConfig {
        lifecycle: LifecycleMode::Offline,
        ..Default::default()
    };
    accounts_config.db.snapshot_frequency = 2;

    let validator_config = millis_per_slot
        .map(|ms| ValidatorConfig {
            millis_per_slot: ms,
            ..Default::default()
        })
        .unwrap_or_default();

    let programs = resolve_programs(programs);

    let config = EphemeralConfig {
        ledger: LedgerConfig {
            reset,
            path: Some(ledger_path.display().to_string()),
            size: DEFAULT_LEDGER_SIZE_BYTES,
        },
        accounts: accounts_config.clone(),
        programs,
        validator: validator_config,
        ..Default::default()
    };
    let (default_tmpdir_config, Some(mut validator)) =
        start_validator_with_config(config, &Default::default())
    else {
        panic!("validator should set up correctly");
    };

    let ctx = expect!(IntegrationTestContext::try_new_ephem_only(), validator);
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
    let mut accounts_config = AccountsConfig {
        lifecycle: LifecycleMode::Ephemeral,
        remote: RemoteConfig {
            cluster: RemoteCluster::Custom,
            url: Some(IntegrationTestContext::url_chain().try_into().unwrap()),
            ws_url: None,
        },
        ..Default::default()
    };
    accounts_config.db.snapshot_frequency = 2;

    let programs = resolve_programs(programs);

    let config = EphemeralConfig {
        ledger: LedgerConfig {
            reset,
            path: Some(ledger_path.display().to_string()),
            size: DEFAULT_LEDGER_SIZE_BYTES,
        },
        accounts: accounts_config.clone(),
        programs,
        ..Default::default()
    };

    let (default_tmpdir_config, Some(mut validator)) =
        start_validator_with_config(
            config,
            &LoadedAccounts::with_delegation_program_test_authority(),
        )
    else {
        panic!("validator should set up correctly");
    };

    let ctx = expect!(IntegrationTestContext::try_new(), validator);
    (default_tmpdir_config, validator, ctx)
}

pub fn cleanup(validator: &mut Child) {
    let _ = validator.kill().inspect_err(|e| {
        eprintln!("ERR: Failed to kill validator: {:?}", e);
    });
}

// -----------------
// Transactions and Account Updates
// -----------------
pub fn send_tx_with_payer_ephem(
    ix: Instruction,
    payer: &Keypair,
    validator: &mut Child,
) -> Signature {
    let ctx = expect!(IntegrationTestContext::try_new_ephem_only(), validator);

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
    let ctx = expect!(IntegrationTestContext::try_new(), validator);
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
    let ctx = expect!(IntegrationTestContext::try_new_ephem_only(), validator);

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
    let ctx = expect!(IntegrationTestContext::try_new_chain_only(), validator);

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
    let ctx = expect!(IntegrationTestContext::try_new_ephem_only(), validator);
    let ephem_client = expect!(ctx.try_ephem_client(), validator);
    fetch_counter(payer, ephem_client, validator)
}

pub fn fetch_counter_chain(
    payer: &Pubkey,
    validator: &mut Child,
) -> FlexiCounter {
    let ctx = expect!(IntegrationTestContext::try_new_chain_only(), validator);
    let chain_client = expect!(ctx.try_chain_client(), validator);
    fetch_counter(payer, chain_client, validator)
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
    let ctx = expect!(IntegrationTestContext::try_new_chain_only(), validator);
    let (counter, _) = FlexiCounter::pda(payer);
    expect!(ctx.fetch_chain_account_owner(counter), validator)
}

// -----------------
// Slot Advances
// -----------------
/// Waits for sufficient slot advances to guarantee that the ledger for
/// the current slot was persisted
pub fn wait_for_ledger_persist(validator: &mut Child) -> Slot {
    let ctx = expect!(IntegrationTestContext::try_new_ephem_only(), validator);

    // I noticed test flakiness if we just advance to next slot once
    // It seems then the ledger hasn't been fully written by the time
    // we kill the validator and the most recent transactions + account
    // updates are missing.
    // Therefore we ensure to advance 10 slots instead of just one
    let mut advances = 10;
    loop {
        let slot = expect!(ctx.wait_for_next_slot_ephem(), validator);
        if advances == 0 {
            break slot;
        }
        advances -= 1;
    }
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

// -----------------
// Asserts
// -----------------
pub struct State {
    pub count: u64,
    pub updates: u64,
}
pub struct Counter<'a> {
    pub payer: &'a Pubkey,
    pub chain: State,
    pub ephem: State,
}

#[macro_export]
macro_rules! assert_counter_state {
    ($validator:expr, $expected:expr, $label:ident) => {
        let counter_chain =
            $crate::fetch_counter_chain($expected.payer, $validator);
        ::cleanass::assert_eq!(
            counter_chain,
            ::program_flexi_counter::state::FlexiCounter {
                count: $expected.chain.count,
                updates: $expected.chain.updates,
                label: $label.to_string()
            },
            $crate::cleanup($validator)
        );

        let counter_ephem =
            $crate::fetch_counter_ephem($expected.payer, $validator);
        ::cleanass::assert_eq!(
            counter_ephem,
            ::program_flexi_counter::state::FlexiCounter {
                count: $expected.ephem.count,
                updates: $expected.ephem.updates,
                label: $label.to_string()
            },
            $crate::cleanup($validator)
        );
    };
}

pub fn wait_for_cloned_accounts_hydration() {
    // NOTE: account hydration runs in the background _after_ the validator starts up
    // thus we need to wait for that to complete before we can send this transaction
    sleep(Duration::from_secs(5));
}
