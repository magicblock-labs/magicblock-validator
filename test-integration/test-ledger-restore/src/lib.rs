use cleanass::{assert, assert_eq};
use log::*;
use std::{path::Path, process::Child, thread::sleep, time::Duration};

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
    AccountsConfig, EphemeralConfig, LedgerConfig, LedgerResumeStrategy,
    LifecycleMode, ProgramConfig, RemoteCluster, RemoteConfig, ValidatorConfig,
    DEFAULT_LEDGER_SIZE_BYTES,
};
use program_flexi_counter::{
    instruction::{create_delegate_ix, create_init_ix},
    state::FlexiCounter,
};
use solana_rpc_client::rpc_client::RpcClient;
use solana_sdk::{
    clock::Slot,
    instruction::Instruction,
    native_token::LAMPORTS_PER_SOL,
    pubkey::Pubkey,
    signature::{Keypair, Signature},
    signer::Signer,
    transaction::Transaction,
};
use tempfile::TempDir;

pub const TMP_DIR_LEDGER: &str = "TMP_DIR_LEDGER";
pub const SNAPSHOT_FREQUENCY: u64 = 2;

pub const FLEXI_COUNTER_ID: &str =
    "f1exzKGtdeVX3d6UXZ89cY7twiNJe9S5uq84RTA4Rq4";
pub const FLEXI_COUNTER_PUBKEY: Pubkey =
    solana_sdk::pubkey!("f1exzKGtdeVX3d6UXZ89cY7twiNJe9S5uq84RTA4Rq4");

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
    accounts_config.db.snapshot_frequency = SNAPSHOT_FREQUENCY;

    let validator_config = millis_per_slot
        .map(|ms| ValidatorConfig {
            millis_per_slot: ms,
            ..Default::default()
        })
        .unwrap_or_default();

    let programs = resolve_programs(programs);

    let config = EphemeralConfig {
        ledger: LedgerConfig {
            resume_strategy_config: resume_strategy.into(),
            skip_keypair_match_check,
            path: ledger_path.display().to_string(),
            size: DEFAULT_LEDGER_SIZE_BYTES,
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
    let resume_strategy = if reset {
        LedgerResumeStrategy::Reset {
            slot: 0,
            keep_accounts: false,
        }
    } else {
        LedgerResumeStrategy::Resume { replay: true }
    };
    setup_validator_with_local_remote_and_resume_strategy(
        ledger_path,
        programs,
        resume_strategy,
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
    programs: Option<Vec<ProgramConfig>>,
    resume_strategy: LedgerResumeStrategy,
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
    accounts_config.db.snapshot_frequency = SNAPSHOT_FREQUENCY;

    let programs = resolve_programs(programs);

    let config = EphemeralConfig {
        ledger: LedgerConfig {
            resume_strategy_config: resume_strategy.into(),
            skip_keypair_match_check,
            path: ledger_path.display().to_string(),
            size: DEFAULT_LEDGER_SIZE_BYTES,
        },
        accounts: accounts_config.clone(),
        programs,
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
pub fn init_and_delegate_counter_and_payer(
    ctx: &IntegrationTestContext,
    validator: &mut Child,
    label: &str,
) -> (Keypair, Pubkey) {
    // 1. Airdrop to payer on chain
    let mut keypairs =
        airdrop_accounts_on_chain(ctx, validator, &[2 * LAMPORTS_PER_SOL]);
    let payer = keypairs.drain(0..1).next().unwrap();

    // 2. Init counter instruction on chain
    let ix = create_init_ix(payer.pubkey(), label.to_string());
    confirm_tx_with_payer_chain(ix, &payer, validator);

    // 3 Delegate counter PDA
    let ix = create_delegate_ix(payer.pubkey());
    confirm_tx_with_payer_chain(ix, &payer, validator);

    // 4. Now we can delegate the payer to use for counter instructions
    //    in the ephemeral
    delegate_accounts(ctx, validator, &[&payer]);

    // 4. Verify all accounts are initialized correctly
    let (counter_pda, _) = FlexiCounter::pda(&payer.pubkey());
    let counter = fetch_counter_chain(&payer.pubkey(), validator);
    assert_eq!(
        counter,
        FlexiCounter {
            count: 0,
            updates: 0,
            label: label.to_string()
        },
        cleanup(validator)
    );

    let payer_chain =
        expect!(ctx.fetch_chain_account(payer.pubkey()), validator);
    assert_eq!(payer_chain.owner, dlp::id(), cleanup(validator));
    assert!(payer_chain.lamports > LAMPORTS_PER_SOL, cleanup(validator));
    debug!(
        "✅ Initialized counter {counter_pda} and delegated payer {}",
        payer.pubkey()
    );

    (payer, counter_pda)
}

pub fn airdrop_accounts_on_chain(
    ctx: &IntegrationTestContext,
    validator: &mut Child,
    lamports: &[u64],
) -> Vec<Keypair> {
    let mut payers = vec![];
    for l in lamports.iter() {
        let payer_chain = Keypair::new();
        expect!(ctx.airdrop_chain(&payer_chain.pubkey(), *l), validator);
        payers.push(payer_chain);
    }
    payers
}

pub fn delegate_accounts(
    ctx: &IntegrationTestContext,
    validator: &mut Child,
    keypairs: &[&Keypair],
) {
    let payer_chain = Keypair::new();
    expect!(
        ctx.airdrop_chain(&payer_chain.pubkey(), LAMPORTS_PER_SOL),
        validator
    );
    for keypair in keypairs.iter() {
        expect!(
            ctx.delegate_account(&payer_chain, keypair),
            format!("Failed to delegate keypair {}", keypair.pubkey()),
            validator
        );
    }
}

pub fn airdrop_and_delegate_accounts(
    ctx: &IntegrationTestContext,
    validator: &mut Child,
    lamports: &[u64],
) -> Vec<Keypair> {
    let payer_chain = Keypair::new();

    let total_lamports: u64 = lamports.iter().sum();
    let payer_lamports = LAMPORTS_PER_SOL + total_lamports;
    // 1. Airdrop to payer on chain
    expect!(
        ctx.airdrop_chain(&payer_chain.pubkey(), payer_lamports),
        validator
    );
    // 2. Airdrop to ephem payers and delegate them
    let keypairs_lamports = lamports
        .into_iter()
        .map(|&l| (Keypair::new(), l))
        .collect::<Vec<_>>();

    for (keypair, l) in keypairs_lamports.iter() {
        expect!(
            ctx.airdrop_chain_and_delegate(&payer_chain, keypair, *l),
            format!("Failed to airdrop {l} and delegate keypair"),
            validator
        );
    }
    keypairs_lamports
        .into_iter()
        .map(|(k, _)| k)
        .collect::<Vec<_>>()
}

pub fn transfer_lamports(
    ctx: &IntegrationTestContext,
    validator: &mut Child,
    from: &Keypair,
    to: &Pubkey,
    lamports: u64,
) -> Signature {
    let transfer_ix =
        solana_sdk::system_instruction::transfer(&from.pubkey(), to, lamports);
    let (sig, confirmed) = expect!(
        ctx.send_and_confirm_instructions_with_payer_ephem(
            &[transfer_ix],
            from
        ),
        "Failed to send transfer",
        validator
    );

    assert!(confirmed, cleanup(validator));
    sig
}

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
    assert!(confirmed, cleanup(validator), "Should confirm transaction",);
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
    assert!(confirmed, cleanup(validator), "Should confirm transaction");
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
pub fn wait_for_next_slot_after_account_snapshot(
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
