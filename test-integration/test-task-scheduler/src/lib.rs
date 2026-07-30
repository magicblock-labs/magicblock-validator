use std::{
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
    workspace_paths::path_relative_to_workspace,
    IntegrationTestContext,
};
use magicblock_config::{
    config::{LifecycleMode, LoadableProgram, TaskSchedulerConfig},
    types::{crypto::SerdeKeypair, network::Remote, SerdePubkey},
    LeaderParams,
};
use magicblock_program::Pubkey;
use program_schedulecommit::MainAccount;
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, signature::Keypair, signer::Signer,
};
use tempfile::TempDir;

pub const TASK_SCHEDULER_TICK_MILLIS: u64 = 50;

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
        programs: vec![LoadableProgram {
            id: SerdePubkey(program_schedulecommit::ID),
            path: path_relative_to_workspace(
                "target/deploy/program_schedulecommit.so",
            )
            .into(),
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
