use std::sync::Arc;

use integration_test_tools::IntegrationTestContext;
use log::*;
use program_mini::sdk::MiniSdk;
use solana_loader_v4_interface::state::{LoaderV4State, LoaderV4Status};
use solana_sdk::{native_token::LAMPORTS_PER_SOL, signature::Keypair};
use spl_memo_interface::{instruction as memo_ix, v1 as memo_v1};
use test_chainlink::programs::{
    deploy::{compile_mini, deploy_loader_v4},
    MINIV2, MINIV3, MINIV3_AUTH,
};
use test_kit::{init_logger, Signer};

macro_rules! assert_tx_logs {
    ($ctx:expr, $sig:expr, $msg:expr) => {
        if let Some(logs) = $ctx.fetch_ephemeral_logs($sig) {
            debug!("Logs for tx {}: {:?}", $sig, logs);
            assert!(logs.contains(&format!("Program log: LogMsg: {}", $msg)));
        } else {
            panic!("No logs found for tx {}", $sig);
        }
    };
}

macro_rules! check_logs {
    ($ctx:expr, $sig:expr, $msg:expr) => {
        if let Some(logs) = $ctx.fetch_ephemeral_logs($sig) {
            debug!("Logs for tx {}: {:?}", $sig, logs);
            logs.contains(&format!("Program log: LogMsg: {}", $msg))
        } else {
            panic!("No logs found for tx {}", $sig);
        }
    };
}

macro_rules! check_v4_program_status {
    ($ctx:expr, $program:expr, $expected_auth:expr) => {
        let data = $program
            .data
            .get(0..LoaderV4State::program_data_offset())
            .unwrap()
            .try_into()
            .unwrap();
        let loader_state = unsafe {
            std::mem::transmute::<
                &[u8; LoaderV4State::program_data_offset()],
                &LoaderV4State,
            >(data)
        };
        debug!("LoaderV4State: {:#?}", loader_state);
        assert_eq!(loader_state.status, LoaderV4Status::Deployed);
        assert_eq!(
            loader_state.authority_address_or_next_version,
            $expected_auth
        );
    };
}

#[test]
fn test_clone_memo_v1_loader_program() {
    init_logger!();
    let ctx = IntegrationTestContext::try_new().unwrap();

    let payer = Keypair::new();
    ctx.airdrop_chain_escrowed(&payer, LAMPORTS_PER_SOL)
        .unwrap();
    let msg = "Hello World";
    let ix =
        memo_ix::build_memo(&memo_v1::id(), msg.as_bytes(), &[&payer.pubkey()]);
    let (_sig, found) = ctx
        .send_and_confirm_instructions_with_payer_ephem(&[ix], &payer)
        .unwrap();
    assert!(found);
}

#[test]
fn test_clone_mini_v2_loader_program() {
    init_logger!();
    let ctx = IntegrationTestContext::try_new().unwrap();

    let sdk = MiniSdk::new(MINIV2);
    let payer = Keypair::new();
    ctx.airdrop_chain_escrowed(&payer, LAMPORTS_PER_SOL)
        .unwrap();

    let program = ctx.fetch_ephem_account(MINIV2).unwrap();
    check_v4_program_status!(ctx, program, MINIV2);

    let msg = "Hello World";
    let ix = sdk.log_msg_instruction(&payer.pubkey(), msg);
    let (sig, found) = ctx
        .send_and_confirm_instructions_with_payer_ephem(&[ix], &payer)
        .unwrap();

    assert!(found);
    assert_tx_logs!(ctx, sig, msg);
}

#[test]
fn test_clone_mini_v3_loader_program() {
    init_logger!();
    let ctx = IntegrationTestContext::try_new().unwrap();

    let sdk = MiniSdk::new(MINIV3);
    let payer = Keypair::new();
    ctx.airdrop_chain_escrowed(&payer, LAMPORTS_PER_SOL)
        .unwrap();
    let msg = "Hello World";
    let ix = sdk.log_msg_instruction(&payer.pubkey(), msg);

    let (sig, found) = ctx
        .send_and_confirm_instructions_with_payer_ephem(&[ix], &payer)
        .unwrap();

    assert!(found);
    assert_tx_logs!(ctx, sig, msg);

    let program = ctx.fetch_ephem_account(MINIV3).unwrap();
    check_v4_program_status!(ctx, program, MINIV3_AUTH);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_clone_mini_v4_loader_program_and_upgrade() {
    init_logger!();
    let prog_kp = Keypair::new();
    let auth_kp = Keypair::new();

    let ctx = IntegrationTestContext::try_new().unwrap();

    // Setting up escrowed payer
    let payer = Keypair::new();
    ctx.airdrop_chain_escrowed(&payer, LAMPORTS_PER_SOL)
        .unwrap();

    let sdk = MiniSdk::new(prog_kp.pubkey());

    // Initial deploy and check
    {
        let program_data = compile_mini(&prog_kp, None);
        debug!("Binary size: {}", program_data.len(),);

        let rpc_client = Arc::new(ctx.try_chain_client_async().unwrap());
        deploy_loader_v4(
            rpc_client.clone(),
            &prog_kp,
            &auth_kp,
            &program_data,
            false,
        )
        .await;

        let msg = "Hello World";
        let ix = sdk.log_msg_instruction(&payer.pubkey(), msg);
        let (sig, found) = ctx
            .send_and_confirm_instructions_with_payer_ephem(&[ix], &payer)
            .unwrap();

        assert!(found);
        assert_tx_logs!(ctx, sig, msg);

        let program = ctx.fetch_ephem_account(prog_kp.pubkey()).unwrap();
        check_v4_program_status!(ctx, program, auth_kp.pubkey());
    }

    // Upgrade and check again
    {
        let program_data = compile_mini(&prog_kp, Some(" upgraded"));
        debug!("Binary size: {}", program_data.len(),);

        let rpc_client = Arc::new(ctx.try_chain_client_async().unwrap());
        deploy_loader_v4(
            rpc_client.clone(),
            &prog_kp,
            &auth_kp,
            &program_data,
            false,
        )
        .await;

        const MAX_RETRIES: usize = 20;
        let mut remaining_retries = MAX_RETRIES;
        loop {
            ctx.wait_for_delta_slot_ephem(5).unwrap();

            let bump = remaining_retries - MAX_RETRIES + 1;
            let msg = format!("Hola Mundo {bump}");
            let ix = sdk.log_msg_instruction(&payer.pubkey(), &msg);
            let (sig, found) = ctx
                .send_and_confirm_instructions_with_payer_ephem(&[ix], &payer)
                .unwrap();

            assert!(found);
            if check_logs!(ctx, sig, format!("{msg} upgraded")) {
                break;
            }

            debug!("Upgrade not yet effective, retrying...");
            if remaining_retries == 0 {
                panic!(
                    "Upgrade not effective after maximum retries {MAX_RETRIES}"
                );
            }
            remaining_retries -= 1;
        }

        let program = ctx.fetch_ephem_account(prog_kp.pubkey()).unwrap();
        check_v4_program_status!(ctx, program, auth_kp.pubkey());
    }
}
