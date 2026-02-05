use std::{
    path::Path, process::Child, str::FromStr, thread::sleep, time::Duration,
};

use cleanass::assert_eq;
use integration_test_tools::{
    expect,
    loaded_accounts::LoadedAccounts,
    validator::{
        cleanup, resolve_programs,
        start_magicblock_validator_with_config_struct,
    },
    IntegrationTestContext,
};
use magicblock_config::{
    config::{
        accounts::AccountsDbConfig, ledger::LedgerConfig,
        scheduler::TaskSchedulerConfig, validator::ValidatorConfig,
        LifecycleMode, LoadableProgram,
    },
    consts::DEFAULT_LEDGER_BLOCK_TIME_MS,
    types::{crypto::SerdePubkey, network::Remote, StorageDirectory},
    ValidatorParams,
};
use program_flexi_counter::state::FlexiCounter;
use solana_sdk::{clock::Slot, native_token::LAMPORTS_PER_SOL, pubkey::Pubkey};
use tempfile::TempDir;

pub const TMP_DIR_LEDGER: &str = "TMP_DIR_LEDGER";
pub const SNAPSHOT_FREQUENCY: u64 = 2;

pub const FLEXI_COUNTER_ID: &str =
    "f1exzKGtdeVX3d6UXZ89cY7twiNJe9S5uq84RTA4Rq4";
pub const FLEXI_COUNTER_PUBKEY: Pubkey =
    solana_sdk::pubkey!("f1exzKGtdeVX3d6UXZ89cY7twiNJe9S5uq84RTA4Rq4");

pub fn setup_offline_validator(
    ledger_path: &Path,
    programs: Option<Vec<LoadableProgram>>,
    millis_per_slot: Option<u64>,
    reset_ledger: bool,
    skip_keypair_match_check: bool,
) -> (TempDir, Child, IntegrationTestContext) {
    let accountsdb_config = AccountsDbConfig {
        snapshot_frequency: SNAPSHOT_FREQUENCY,
        ..Default::default()
    };

    let validator_config = ValidatorConfig::default();

    let programs = resolve_programs(programs);

    let config = ValidatorParams {
        ledger: LedgerConfig {
            reset: reset_ledger,
            verify_keypair: !skip_keypair_match_check,
            block_time: millis_per_slot
                .map(Duration::from_millis)
                .unwrap_or_else(|| {
                    Duration::from_millis(DEFAULT_LEDGER_BLOCK_TIME_MS)
                }),
            ..Default::default()
        },
        accountsdb: accountsdb_config.clone(),
        programs,
        validator: validator_config,
        lifecycle: LifecycleMode::Offline,
        storage: StorageDirectory(ledger_path.to_path_buf()),
        ..Default::default()
    };
    let (default_tmpdir_config, Some(mut validator), port) =
        start_magicblock_validator_with_config_struct(
            config,
            &Default::default(),
        )
    else {
        panic!("validator should set up correctly");
    };

    let ctx = expect!(
        IntegrationTestContext::try_new_with_ephem_port(port),
        validator
    );
    (default_tmpdir_config, validator, ctx)
}

/// This function sets up a validator that connects to a local remote.
/// That local remote is expected to listen on port 7799.
/// The [IntegrationTestContext] is setup to connect to both the ephemeral validator
/// and the local remote.
pub fn setup_validator_with_local_remote(
    ledger_path: &Path,
    programs: Option<Vec<LoadableProgram>>,
    reset: bool,
    skip_keypair_match_check: bool,
    loaded_accounts: &LoadedAccounts,
) -> (TempDir, Child, IntegrationTestContext) {
    setup_validator_with_local_remote_and_resume_strategy(
        ledger_path,
        programs,
        reset,
        skip_keypair_match_check,
        loaded_accounts,
    )
}

/// This function sets up a validator that connects to a local remote and allows to
/// specify the resume strategy specifically.
/// That local remote is expected to listen on port 7799.
/// The [IntegrationTestContext] is setup to connect to both the ephemeral validator
/// and the local remote.
pub fn setup_validator_with_local_remote_and_resume_strategy(
    ledger_path: &Path,
    programs: Option<Vec<LoadableProgram>>,
    reset_ledger: bool,
    skip_keypair_match_check: bool,
    loaded_accounts: &LoadedAccounts,
) -> (TempDir, Child, IntegrationTestContext) {
    let accountsdb_config = AccountsDbConfig {
        snapshot_frequency: SNAPSHOT_FREQUENCY,
        reset: reset_ledger,
        ..Default::default()
    };

    let programs = resolve_programs(programs);

    let config = ValidatorParams {
        ledger: LedgerConfig {
            reset: reset_ledger,
            verify_keypair: !skip_keypair_match_check,
            ..Default::default()
        },
        accountsdb: accountsdb_config.clone(),
        programs,
        task_scheduler: TaskSchedulerConfig {
            reset: true,
            ..Default::default()
        },
        lifecycle: LifecycleMode::Ephemeral,
        remotes: vec![
            Remote::from_str(IntegrationTestContext::url_chain()).unwrap(),
            Remote::from_str(IntegrationTestContext::ws_url_chain()).unwrap(),
        ],
        storage: StorageDirectory(ledger_path.to_path_buf()),
        ..Default::default()
    };
    // Fund validator on chain
    {
        let chain_only_ctx =
            IntegrationTestContext::try_new_chain_only().unwrap();
        chain_only_ctx
            .airdrop_chain(
                &loaded_accounts.validator_authority(),
                20 * LAMPORTS_PER_SOL,
            )
            .unwrap();
    }

    let (default_tmpdir_config, Some(mut validator), port) =
        start_magicblock_validator_with_config_struct(config, loaded_accounts)
    else {
        panic!("validator should set up correctly");
    };

    let ctx = expect!(
        IntegrationTestContext::try_new_with_ephem_port(port),
        validator
    );
    (default_tmpdir_config, validator, ctx)
}

// -----------------
// Slot Advances
// -----------------
/// Waits for sufficient slot advances to guarantee that the ledger for
/// the current slot was persisted
pub fn wait_for_ledger_persist(
    ctx: &IntegrationTestContext,
    validator: &mut Child,
) -> Slot {
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

/// Waits for the next slot after the snapshot frequency
pub fn wait_for_snapshot(
    validator: &mut Child,
    snapshot_frequency: u64,
) -> Slot {
    let ctx = expect!(IntegrationTestContext::try_new_ephem_only(), validator);

    let initial_slot = expect!(ctx.get_slot_ephem(), validator);
    let slots_until_next_snapshot =
        snapshot_frequency - (initial_slot % snapshot_frequency);

    expect!(
        ctx.wait_for_delta_slot_ephem(slots_until_next_snapshot + 1),
        validator
    )
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
    assert_eq!(stats.len(), expected_count, cleanup(validator));
}

// -----------------
// Configs
// -----------------
pub fn get_programs_with_flexi_counter() -> Vec<LoadableProgram> {
    vec![LoadableProgram {
        id: SerdePubkey(FLEXI_COUNTER_PUBKEY),
        path: "program_flexi_counter.so".into(),
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
    ($ctx:expr, $validator:expr, $expected:expr, $label:ident) => {
        let counter_chain =
            ::integration_test_tools::scenario_setup::fetch_counter_chain(
                $expected.payer,
                $validator,
            );
        ::cleanass::assert_eq!(
            counter_chain,
            ::program_flexi_counter::state::FlexiCounter {
                count: $expected.chain.count,
                updates: $expected.chain.updates,
                label: $label.to_string()
            },
            ::integration_test_tools::validator::cleanup($validator)
        );

        let counter_ephem =
            ::integration_test_tools::scenario_setup::fetch_counter_ephem(
                $ctx,
                $expected.payer,
                $validator,
            );
        ::cleanass::assert_eq!(
            counter_ephem,
            ::program_flexi_counter::state::FlexiCounter {
                count: $expected.ephem.count,
                updates: $expected.ephem.updates,
                label: $label.to_string()
            },
            ::integration_test_tools::validator::cleanup($validator)
        );
    };
}

pub fn wait_for_cloned_accounts_hydration() {
    // NOTE: account hydration runs in the background _after_ the validator starts up
    // thus we need to wait for that to complete before we can send this transaction
    sleep(Duration::from_secs(5));
}

/// Waits for the next slot after the snapshot frequency
pub fn wait_for_next_slot_after_account_snapshot(
    ctx: &IntegrationTestContext,
    validator: &mut Child,
    snapshot_frequency: u64,
) -> Slot {
    let initial_slot = expect!(ctx.get_slot_ephem(), validator);
    let slots_until_next_snapshot =
        snapshot_frequency - (initial_slot % snapshot_frequency);

    expect!(
        ctx.wait_for_delta_slot_ephem(slots_until_next_snapshot + 1),
        validator
    )
}
