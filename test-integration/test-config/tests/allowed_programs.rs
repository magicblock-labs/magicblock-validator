use std::str::FromStr;

use integration_test_tools::{
    expect, expect_err, init_logger,
    loaded_accounts::LoadedAccounts,
    validator::{cleanup, start_magicblock_validator_with_config_struct},
    IntegrationTestContext,
};
use magicblock_config::{
    config::{AllowedProgram, ChainLinkConfig, LifecycleMode},
    types::network::Remote,
    LeaderParams,
};
use serial_test::file_serial;
use solana_sdk::pubkey::Pubkey;
use tracing::*;
use v42_calculator_interface::ID as V42_ID;

#[test]
#[file_serial]
fn test_allowed_programs_restricts_cloning() {
    run_allowed_programs(false);
}

#[test]
#[file_serial]
fn test_allowed_programs_none_restricts_no_cloning() {
    run_allowed_programs(true);
}

fn run_allowed_programs(allow_committor_program: bool) {
    init_logger!();

    let allowed_programs =
        (!allow_committor_program).then(|| vec![AllowedProgram { id: V42_ID }]);

    // Create another random program ID that we'll block
    // we use the commmittor helper program ID as an example since that is
    // present in the chain validator
    let blocked_program_id =
        Pubkey::from_str("ComtrB2KEaWgXsW1dhr1xYL4Ht4Bjj3gXnnL6KMdABq")
            .unwrap();

    let config = LeaderParams {
        programs: vec![],
        lifecycle: LifecycleMode::Ephemeral,
        remotes: vec![
            Remote::from_str(IntegrationTestContext::url_chain()).unwrap(),
            Remote::from_str(IntegrationTestContext::ws_url_chain()).unwrap(),
        ],
        chainlink: ChainLinkConfig {
            // Only allow the v42 program and block the other one.
            allowed_programs,
            ..Default::default()
        },
        ..Default::default()
    };

    let (_default_tmpdir, Some(mut validator), port) =
        start_magicblock_validator_with_config_struct(
            config,
            &LoadedAccounts::with_delegation_program_test_authority(),
        )
    else {
        panic!("validator should set up correctly");
    };

    let ctx = expect!(
        IntegrationTestContext::try_new_with_ephem_port(port),
        validator
    );

    // Give validator another moment to fully startup
    std::thread::sleep(std::time::Duration::from_secs(1));

    // We request both account infos and then verify that only the allowed one
    // is cloned if one was blocked
    expect!(
        ctx.fetch_ephem_account(V42_ID),
        "v42 program should be cloned successfully",
        validator
    );
    debug!("✅ Allowed program cloned successfully");

    if allow_committor_program {
        expect!(
            ctx.fetch_ephem_account(blocked_program_id),
            "committor program should be cloned successfully",
            validator
        );
        debug!("✅ Second allowed program cloned successfully");
    } else {
        expect_err!(
            ctx.fetch_ephem_account(blocked_program_id),
            "blocked program should not be cloned",
            validator
        );
        debug!("✅ Blocked program was not cloned as expected");
    }

    cleanup(&mut validator);
}
