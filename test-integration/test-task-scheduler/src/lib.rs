use std::{
    path::PathBuf,
    process::Child,
    str::FromStr,
    time::{Duration, Instant},
};

use cleanass::assert;
use hydra_api::ephemeral::ID as HYDRA_EPHEMERAL_PROGRAM_ID;
use integration_test_tools::{
    expect,
    loaded_accounts::LoadedAccounts,
    tmpdir::resolve_tmp_dir,
    validator::{
        cleanup, start_magicblock_validator_with_config_struct_and_temp_dir,
        TMP_DIR_CONFIG,
    },
    IntegrationTestContext,
};
use magicblock_config::{
    config::{LifecycleMode, LoadableProgram, TaskSchedulerConfig},
    types::{crypto::SerdeKeypair, network::Remote, SerdePubkey},
    LeaderParams,
};
use magicblock_program::{
    args::ScheduleTaskArgs, instruction_utils::InstructionUtils, Pubkey,
};
use program_schedulecommit::MainAccount;
use solana_sdk::{
    instruction::Instruction, native_token::LAMPORTS_PER_SOL,
    signature::Keypair, signer::Signer, transaction::Transaction,
};
use tempfile::TempDir;

pub const TASK_SCHEDULER_TICK_MILLIS: u64 = 50;

/// Absolute path to the ephemeral hydra program the ER preloads.
fn hydra_program_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../programs/hydra/hydra.so")
}

fn airdrop_faucet(faucet: &Keypair) {
    let chain_ctx = IntegrationTestContext::try_new_chain_only()
        .expect("failed to connect to base chain to fund faucet");
    chain_ctx
        .airdrop_chain(&faucet.pubkey(), 100 * LAMPORTS_PER_SOL)
        .expect("failed to airdrop to task scheduler faucet");
}

/// Starts a validator whose task scheduler sponsors hydra cranks from a freshly
/// funded faucet. The faucet must be funded before startup, because the
/// validator delegates it (but never funds it) while coming up.
pub fn setup_validator() -> (TempDir, Child, IntegrationTestContext, Keypair) {
    let (default_tmpdir, temp_dir) = resolve_tmp_dir(TMP_DIR_CONFIG);

    let faucet = Keypair::new();
    airdrop_faucet(&faucet);

    let config = LeaderParams {
        lifecycle: LifecycleMode::Ephemeral,
        remotes: vec![
            Remote::from_str(IntegrationTestContext::url_chain()).unwrap(),
            Remote::from_str(IntegrationTestContext::ws_url_chain()).unwrap(),
        ],
        task_scheduler: TaskSchedulerConfig {
            faucet_keypair: Some(
                SerdeKeypair::from_str(&faucet.to_base58_string()).unwrap(),
            ),
        },
        // `eHyd5…` is the *ephemeral* hydra program: it executes cranks inside
        // the ER, so it is preloaded here rather than cloned from the base
        // chain (which only hosts hydra's base-chain counterpart).
        programs: vec![LoadableProgram {
            id: SerdePubkey(HYDRA_EPHEMERAL_PROGRAM_ID),
            path: hydra_program_path(),
        }],
        ..Default::default()
    };
    let (default_tmpdir_config, Some(mut validator), port) =
        start_magicblock_validator_with_config_struct_and_temp_dir(
            config,
            &LoadedAccounts::with_delegation_program_test_authority(),
            default_tmpdir,
            temp_dir,
        )
    else {
        panic!("validator should set up correctly");
    };
    let ctx = expect!(
        IntegrationTestContext::try_new_with_ephem_port(port),
        validator
    );
    (default_tmpdir_config, validator, ctx, faucet)
}

/// Sends an ephemeral transaction and asserts it committed successfully.
pub fn send_ephem_tx(
    ctx: &IntegrationTestContext,
    validator: &mut Child,
    instructions: &[Instruction],
    payer: &Keypair,
) {
    let blockhash = expect!(ctx.try_get_latest_blockhash_ephem(), validator);
    // Confirm rather than just send: the caller's next step usually reads state
    // this transaction produced, which would otherwise race the ledger.
    let (signature, confirmed) = expect!(
        ctx.send_and_confirm_transaction_ephem(
            &mut Transaction::new_signed_with_payer(
                instructions,
                Some(&payer.pubkey()),
                &[payer],
                blockhash,
            ),
            &[payer]
        ),
        validator
    );
    assert!(
        confirmed,
        cleanup(validator),
        "ephemeral transaction {} was not confirmed", signature
    );
}

/// The payload a scheduled crank carries in these tests: a single magic-program
/// noop.
///
/// Cranks reject scheduled instructions that declare signers, so the payload
/// cannot be a transfer or any other instruction that needs authorization. A
/// noop keeps the tests focused on the scheduler and hydra — crank creation,
/// funding, execution and teardown — rather than on a payload program's state.
pub fn noop_task_instructions() -> Vec<Instruction> {
    vec![InstructionUtils::noop_instruction(0)]
}

/// Schedules a recurring noop task through the magic program, as any user
/// program would. The task scheduler observes the request and creates the
/// matching hydra crank.
pub fn schedule_noop_task(
    ctx: &IntegrationTestContext,
    validator: &mut Child,
    payer: &Keypair,
    task_id: i64,
    execution_interval_millis: i64,
    iterations: i64,
) {
    send_ephem_tx(
        ctx,
        validator,
        &[InstructionUtils::schedule_task_instruction(
            &payer.pubkey(),
            ScheduleTaskArgs {
                task_id,
                execution_interval_millis,
                iterations,
                instructions: noop_task_instructions(),
            },
        )],
        payer,
    );
}

/// Cancels a previously scheduled task through the magic program.
pub fn cancel_task(
    ctx: &IntegrationTestContext,
    validator: &mut Child,
    payer: &Keypair,
    task_id: i64,
) {
    send_ephem_tx(
        ctx,
        validator,
        &[InstructionUtils::cancel_task_instruction(
            &payer.pubkey(),
            task_id,
        )],
        payer,
    );
}

/// Waits until the crank faucet is delegated and visible inside the ER.
///
/// The validator delegates the faucet in a background startup task, so a test
/// that samples the faucet balance must not race that delegation.
pub fn wait_for_funded_faucet(
    ctx: &IntegrationTestContext,
    faucet: &Pubkey,
    max_timeout: Duration,
    validator: &mut Child,
) -> u64 {
    let now = Instant::now();
    while now.elapsed() < max_timeout {
        if let Ok(balance) = ctx.fetch_ephem_account_balance(faucet) {
            if balance > 0 {
                return balance;
            }
        }
        expect!(ctx.wait_for_next_slot_ephem(), validator);
    }
    assert!(
        false,
        cleanup(validator),
        "crank faucet {} was not funded in the ER before timeout", faucet
    );
    unreachable!()
}

/// Waits until the hydra crank account for a task exists and is hydra-owned.
pub fn wait_for_hydra_crank(
    ctx: &IntegrationTestContext,
    crank_pda: &Pubkey,
    max_timeout: Duration,
    validator: &mut Child,
) {
    let now = Instant::now();
    while now.elapsed() < max_timeout {
        let maybe_account = ctx
            .try_ephem_client()
            .ok()
            .and_then(|client| client.get_account(crank_pda).ok());
        if let Some(account) = maybe_account {
            assert!(
                account.owner.to_bytes()
                    == HYDRA_EPHEMERAL_PROGRAM_ID.to_bytes(),
                cleanup(validator),
                "crank account {} not owned by hydra program (owner: {})",
                crank_pda,
                account.owner
            );
            return;
        }
        expect!(ctx.wait_for_next_slot_ephem(), validator);
    }
    assert!(
        false,
        cleanup(validator),
        "hydra crank account {} was not created before timeout", crank_pda
    );
}

/// Waits until the hydra crank account for a task has been closed (cancelled).
pub fn wait_for_hydra_crank_closed(
    ctx: &IntegrationTestContext,
    crank_pda: &Pubkey,
    max_timeout: Duration,
    validator: &mut Child,
) {
    let now = Instant::now();
    while now.elapsed() < max_timeout {
        let client = expect!(ctx.try_ephem_client(), validator);
        let account = expect!(
            client.get_account_with_commitment(crank_pda, ctx.commitment),
            validator
        )
        .value;
        if account.is_none_or(|account| account.lamports == 0) {
            return;
        }
        expect!(ctx.wait_for_next_slot_ephem(), validator);
    }
    assert!(
        false,
        cleanup(validator),
        "hydra crank account {} was not closed before timeout", crank_pda
    );
}

pub fn wait_for_committed_count(
    ctx: &IntegrationTestContext,
    committee: &Pubkey,
    expected_count: u64,
    max_timeout: Duration,
    validator: &mut Child,
) {
    let now = Instant::now();
    while now.elapsed() < max_timeout {
        let account = expect!(
            ctx.try_chain_client().and_then(|client| client
                .get_account(committee)
                .map_err(|err| anyhow::anyhow!(
                    "failed to get chain account: {err}"
                ))),
            validator
        );
        if let Ok(state) = MainAccount::try_decode(&account.data) {
            if state.count == expected_count {
                return;
            }
        }
        expect!(ctx.wait_for_next_slot_ephem(), validator);
    }
    assert!(
        false,
        cleanup(validator),
        "task did not commit the expected count"
    );
}
