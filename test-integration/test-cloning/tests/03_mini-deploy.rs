use std::sync::Arc;

use integration_test_tools::IntegrationTestContext;
use log::*;
use program_mini::sdk::MiniSdk;
use solana_sdk::{native_token::LAMPORTS_PER_SOL, signature::Keypair};
use test_chainlink::programs::{
    deploy::{compile_mini, deploy_loader_v4},
    MINIV2, MINIV3,
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_clone_mini_v2_loader_program() {
    init_logger!();
    let ctx = IntegrationTestContext::try_new().unwrap();

    let sdk = MiniSdk::new(MINIV2);
    let payer = Keypair::new();
    ctx.airdrop_chain_escrowed(&payer, LAMPORTS_PER_SOL)
        .await
        .unwrap();
    let msg = "Hello World";
    let ix = sdk.log_msg_instruction(&payer.pubkey(), msg);
    let (sig, found) = ctx
        .send_and_confirm_instructions_with_payer_ephem(&[ix], &payer)
        .unwrap();

    assert!(found);
    assert_tx_logs!(ctx, sig, msg);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_clone_mini_v3_loader_program() {
    init_logger!();
    let ctx = IntegrationTestContext::try_new().unwrap();

    let sdk = MiniSdk::new(MINIV3);
    let payer = Keypair::new();
    ctx.airdrop_chain_escrowed(&payer, LAMPORTS_PER_SOL)
        .await
        .unwrap();
    let msg = "Hello World";
    let ix = sdk.log_msg_instruction(&payer.pubkey(), msg);

    let (sig, found) = ctx
        .send_and_confirm_instructions_with_payer_ephem(&[ix], &payer)
        .unwrap();

    assert!(found);
    assert_tx_logs!(ctx, sig, msg);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_clone_mini_v4_loader_program() {
    init_logger!();
    let prog_kp = Keypair::new();
    let auth_kp = Keypair::new();

    let ctx = IntegrationTestContext::try_new().unwrap();

    let program_data = compile_mini(&prog_kp);
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

    let sdk = MiniSdk::new(prog_kp.pubkey());
    let payer = Keypair::new();
    ctx.airdrop_chain_escrowed(&payer, LAMPORTS_PER_SOL)
        .await
        .unwrap();
    let msg = "Hello World";
    let ix = sdk.log_msg_instruction(&payer.pubkey(), msg);
    let (sig, found) = ctx
        .send_and_confirm_instructions_with_payer_ephem(&[ix], &payer)
        .unwrap();

    assert!(found);
    assert_tx_logs!(ctx, sig, msg);
}
