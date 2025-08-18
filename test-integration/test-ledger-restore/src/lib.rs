use solana_rpc_client::rpc_client::RpcClient;
use std::{path::Path, process::Child, thread::sleep, time::Duration};

use integration_test_tools::{
    expect,
    loaded_accounts::LoadedAccounts,
    validator::{
        resolve_programs, start_magicblock_validator_with_config_struct,
    },
    IntegrationTestContext,
};
use magicblock_config::{
    AccountsConfig, EphemeralConfig, LedgerConfig, LedgerResumeStrategy,
    LifecycleMode, ProgramConfig, RemoteCluster, RemoteConfig, ValidatorConfig,
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

pub const FLEXI_COUNTER_ID: &str =
    "f1exzKGtdeVX3d6UXZ89cY7twiNJe9S5uq84RTA4Rq4";
pub const FLEXI_COUNTER_PUBKEY: Pubkey =
    pubkey!("f1exzKGtdeVX3d6UXZ89cY7twiNJe9S5uq84RTA4Rq4");

pub fn setup_offline_validator(
    ledger_path: &Path,
    programs: Option<Vec<ProgramConfig>>,
    millis_per_slot: Option<u64>,
    resume_strategy: LedgerResumeStrategy,
    skip_keypair_match_check: bool,
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
            resume_strategy,
            skip_keypair_match_check,
            path: Some(ledger_path.display().to_string()),
            size: DEFAULT_LEDGER_SIZE_BYTES,
            ..Default::default()
        },
        accounts: accounts_config.clone(),
        programs,
        validator: validator_config,
        ..Default::default()
    };
    let (default_tmpdir_config, Some(mut validator)) =
        start_magicblock_validator_with_config_struct(
            config,
            &Default::default(),
        )
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
    skip_keypair_match_check: bool,
    loaded_accounts: &LoadedAccounts,
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

    let resume_strategy = if reset {
        LedgerResumeStrategy::Reset
    } else {
        LedgerResumeStrategy::Replay
    };
    let config = EphemeralConfig {
        ledger: LedgerConfig {
            resume_strategy,
            skip_keypair_match_check,
            path: Some(ledger_path.display().to_string()),
            size: DEFAULT_LEDGER_SIZE_BYTES,
        },
        accounts: accounts_config.clone(),
        programs,
        ..Default::default()
    };

    let (default_tmpdir_config, Some(mut validator)) =
        start_magicblock_validator_with_config_struct(config, loaded_accounts)
    else {
        panic!("validator should set up correctly");
    };

    let ctx = expect!(IntegrationTestContext::try_new(), validator);
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
            ::integration_test_tools::validator::cleanup($validator)
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
