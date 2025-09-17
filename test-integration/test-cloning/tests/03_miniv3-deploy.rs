use std::sync::Arc;

use integration_test_tools::IntegrationTestContext;
use log::*;
use program_mini::sdk::MiniSdk;
use solana_sdk::{native_token::LAMPORTS_PER_SOL, signature::Keypair};
use test_chainlink::programs::{
    deploy::{compile_mini, deploy_loader_v4},
    MINIV3,
};
use test_kit::{init_logger, Signer};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_clone_mini_v3_loader_program() {
    init_logger!();
    let ctx = IntegrationTestContext::try_new().unwrap();

    let program_acc = ctx
        .fetch_ephem_account(MINIV3)
        .expect("failed to fetch mini v3 loader program account");
    debug!("{:#?}", program_acc);

    let sdk = MiniSdk::new(MINIV3);
    let payer = Keypair::new();
    ctx.airdrop_chain_escrowed(&payer, LAMPORTS_PER_SOL)
        .await
        .unwrap();
    let ix = sdk.log_msg_instruction(&payer.pubkey(), "Hello World");
    ctx.send_and_confirm_instructions_with_payer_ephem(&[ix], &payer)
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_clone_mini_v4_loader_program() {
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
}
