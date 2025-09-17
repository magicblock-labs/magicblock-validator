use std::sync::Arc;

use magicblock_chainlink::{
    assert_loaded_program_with_size,
    assert_subscribed_without_loaderv3_program_data_account,
    remote_account_provider::program_account::{
        LoadedProgram, ProgramAccountResolver, RemoteProgramLoader,
    },
    testing::init_logger,
};

use log::*;
use program_mini::common::IdlType;
use solana_loader_v4_interface::state::LoaderV4Status;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig, signature::Keypair, signer::Signer,
};
use test_chainlink::{
    assert_program_owned_by_loader, fetch_and_assert_loaded_program_v1_v2_v4,
    fetch_and_assert_loaded_program_v3, mini_upload_idl,
    programs::{
        airdrop_sol,
        deploy::{compile_mini, deploy_loader_v4},
        mini::{load_miniv2_so, load_miniv3_so},
        send_instructions, MEMOV1, MEMOV2, MINIV2, MINIV3, MINIV3_AUTH,
        OTHERV1,
    },
    test_mini_program, test_mini_program_log_msg,
};

use test_chainlink::{ixtest_context::IxtestContext, programs::memo};

const RPC_URL: &str = "http://localhost:7799";
fn get_rpc_client(commitment: CommitmentConfig) -> RpcClient {
    RpcClient::new_with_commitment(RPC_URL.to_string(), commitment)
}

fn pretty_bytes(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.2} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.2} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

// -----------------
// Fetching, deserializing and redeploying programs
// -----------------
#[tokio::test]
async fn ixtest_fetch_memo_v1_loader_program() {
    init_logger();

    // NOTE: one cannot load a newer program into the v1 loader and
    // have execute transactions properly
    // Thus we use the memo program for this loader

    let auth_kp = Keypair::new();
    let commitment = CommitmentConfig::processed();
    let rpc_client = Arc::new(get_rpc_client(commitment));

    assert_program_owned_by_loader!(&rpc_client, &MEMOV1, 1);

    airdrop_sol(&rpc_client, &auth_kp.pubkey(), 10).await;

    // 1. Ensure that the program works on the remote
    let ix =
        memo::build_memo(&MEMOV1, b"This is a test memo", &[&auth_kp.pubkey()]);
    let sig =
        send_instructions(&rpc_client, &[ix], &[&auth_kp], "test_memo").await;
    debug!("Memo instruction sent, signature: {sig}");

    // 2. Ensure we can directly deserialize the program account
    let program_account = rpc_client
        .get_account(&MEMOV1)
        .await
        .expect("Failed to get program account");
    let loaded_program = fetch_and_assert_loaded_program_v1_v2_v4!(
        &rpc_client,
        MEMOV1,
        LoadedProgram {
            program_id: MEMOV1,
            authority: MEMOV1,
            program_data: program_account.data,
            loader: RemoteProgramLoader::V1,
            loader_status: LoaderV4Status::Finalized,
            remote_slot: 0
        }
    );

    // 3. Redeploy with v4 loader and show it fails
    // Deploy via v3 fails as well and we cannot _officiallY_ deploy via
    // the v1 or v2 loaders

    let prog_kp = Keypair::new();
    let auth_kp = Keypair::new();
    let program_data = loaded_program.program_data;
    deploy_loader_v4(
        rpc_client.clone(),
        &prog_kp,
        &auth_kp,
        &program_data,
        true,
    )
    .await;
    debug!(
        "Memo v1 redeploy V4 failed for: {} as expected",
        prog_kp.pubkey()
    );
}

#[tokio::test]
async fn ixtest_fetch_other_v1_loader_program() {
    // This test shows that no v1 program will fail to redeploy with v4 loader
    // Not only the Memo V1
    init_logger();

    let auth_kp = Keypair::new();
    let commitment = CommitmentConfig::processed();
    let rpc_client = Arc::new(get_rpc_client(commitment));

    assert_program_owned_by_loader!(&rpc_client, &OTHERV1, 1);

    airdrop_sol(&rpc_client, &auth_kp.pubkey(), 10).await;

    // 1. Ensure we can directly deserialize the program account
    let program_account = rpc_client
        .get_account(&OTHERV1)
        .await
        .expect("Failed to get program account");
    let loaded_program = fetch_and_assert_loaded_program_v1_v2_v4!(
        &rpc_client,
        OTHERV1,
        LoadedProgram {
            program_id: OTHERV1,
            authority: OTHERV1,
            program_data: program_account.data,
            loader: RemoteProgramLoader::V1,
            loader_status: LoaderV4Status::Finalized,
            remote_slot: 0
        }
    );

    let prog_kp = Keypair::new();
    let auth_kp = Keypair::new();
    let program_data = loaded_program.program_data;
    deploy_loader_v4(
        rpc_client.clone(),
        &prog_kp,
        &auth_kp,
        &program_data,
        true,
    )
    .await;
    debug!(
        "Program redeploy V4 failed for: {} as expected",
        prog_kp.pubkey()
    );
}

#[tokio::test]
async fn ixtest_fetch_memo_v2_loader_program_memo_v2() {
    init_logger();

    // The main point of this test is to show that we can load a v2 program
    // that was bpf compiled into a v4 loader

    let auth_kp = Keypair::new();
    let commitment = CommitmentConfig::processed();
    let rpc_client = Arc::new(get_rpc_client(commitment));

    assert_program_owned_by_loader!(&rpc_client, &MEMOV1, 1);

    airdrop_sol(&rpc_client, &auth_kp.pubkey(), 10).await;

    // 1. Ensure that the program works on the remote
    let ix = memo::build_memo(
        &MEMOV2,
        b"This is a test memo for v2",
        &[&auth_kp.pubkey()],
    );
    let sig =
        send_instructions(&rpc_client, &[ix], &[&auth_kp], "test_memo_v2")
            .await;
    debug!("Memo v2 instruction sent, signature: {sig}");

    // 2. Ensure we can directly deserialize the program account
    let program_account = rpc_client
        .get_account(&MEMOV2)
        .await
        .expect("Failed to get program account");
    let loaded_program = fetch_and_assert_loaded_program_v1_v2_v4!(
        &rpc_client,
        MEMOV2,
        LoadedProgram {
            program_id: MEMOV2,
            authority: MEMOV2,
            program_data: program_account.data,
            loader: RemoteProgramLoader::V2,
            loader_status: LoaderV4Status::Finalized,
            remote_slot: 0
        }
    );

    // 3. Redeploy with v4 loader and ensure it works
    let prog_kp = Keypair::new();
    let auth_kp = Keypair::new();
    let program_data = loaded_program.program_data;
    deploy_loader_v4(
        rpc_client.clone(),
        &prog_kp,
        &auth_kp,
        &program_data,
        false,
    )
    .await;

    let ix = memo::build_memo(
        &prog_kp.pubkey(),
        b"This is a test memo for redeployed v2",
        &[&auth_kp.pubkey()],
    );
    let sig = send_instructions(
        &rpc_client,
        &[ix],
        &[&auth_kp],
        "redeploy:test_memo_v2",
    )
    .await;
    debug!("Memo redeploy v2 instruction sent, signature: {sig}");
}

#[tokio::test]
async fn ixtest_fetch_mini_v2_loader_program() {
    init_logger();

    let auth_kp = Keypair::new();
    let commitment = CommitmentConfig::processed();
    let rpc_client = Arc::new(get_rpc_client(commitment));

    assert_program_owned_by_loader!(&rpc_client, &MINIV2, 2);

    airdrop_sol(&rpc_client, &auth_kp.pubkey(), 20).await;

    // 1. Ensure that the program works on the remote
    test_mini_program!(&rpc_client, &MINIV2, &auth_kp);

    // 2. Ensure we can directly deserialize the program account
    let mini_so = load_miniv2_so();
    let loaded_program = fetch_and_assert_loaded_program_v1_v2_v4!(
        &rpc_client,
        MINIV2,
        LoadedProgram {
            program_id: MINIV2,
            authority: MINIV2,
            program_data: mini_so,
            loader: RemoteProgramLoader::V2,
            loader_status: LoaderV4Status::Finalized,
            remote_slot: 0
        }
    );
    // 3. Redeploy with v4 loader and ensure it works
    let prog_kp = Keypair::new();
    let auth_kp = Keypair::new();
    let program_data = loaded_program.program_data;
    deploy_loader_v4(
        rpc_client.clone(),
        &prog_kp,
        &auth_kp,
        &program_data,
        false,
    )
    .await;
    test_mini_program_log_msg!(
        &rpc_client,
        &prog_kp.pubkey(),
        &auth_kp,
        "Hello new deployment"
    );

    // 4. Upload a shank IDL for the program
    let idl = b"Mini Program V2 IDL";
    mini_upload_idl!(&rpc_client, &auth_kp, &MINIV2, IdlType::Shank, idl);
}

#[tokio::test]
async fn ixtest_fetch_mini_v3_loader_program() {
    init_logger();

    let auth_kp = Keypair::new();
    let commitment = CommitmentConfig::processed();
    let rpc_client = Arc::new(get_rpc_client(commitment));

    assert_program_owned_by_loader!(&rpc_client, &MINIV3, 3);

    airdrop_sol(&rpc_client, &auth_kp.pubkey(), 20).await;

    // 1. Ensure that the program works on the remote
    test_mini_program!(&rpc_client, &MINIV3, &auth_kp);

    // 2. Ensure we can directly deserialize the program account
    let mini_so = load_miniv3_so();
    let loaded_program = fetch_and_assert_loaded_program_v3!(
        rpc_client,
        MINIV3,
        LoadedProgram {
            program_id: MINIV3,
            authority: MINIV3_AUTH,
            program_data: mini_so,
            loader: RemoteProgramLoader::V3,
            loader_status:
                solana_loader_v4_interface::state::LoaderV4Status::Deployed,
            remote_slot: 0
        }
    );

    // 3. Redeploy with v4 loader and ensure it works
    let prog_kp = Keypair::new();
    let auth_kp = Keypair::new();
    let program_data = loaded_program.program_data;
    deploy_loader_v4(
        rpc_client.clone(),
        &prog_kp,
        &auth_kp,
        &program_data,
        false,
    )
    .await;
    test_mini_program_log_msg!(
        &rpc_client,
        &prog_kp.pubkey(),
        &auth_kp,
        "Hello new deployment"
    );

    // 4. Upload a anchor IDL for the program and update it
    let idl = b"Mini Program V3 IDL V1";
    mini_upload_idl!(&rpc_client, &auth_kp, &MINIV3, IdlType::Anchor, idl);

    let idl = b"Mini Program V3 IDL V2";
    mini_upload_idl!(&rpc_client, &auth_kp, &MINIV3, IdlType::Anchor, idl);
}

#[tokio::test]
async fn ixtest_fetch_mini_v4_loader_program() {
    init_logger();

    let prog_kp = Keypair::new();
    let auth_kp = Keypair::new();

    let program_data = compile_mini(&prog_kp, None);
    debug!(
        "Binary size: {} ({})",
        pretty_bytes(program_data.len()),
        program_data.len()
    );

    let commitment = CommitmentConfig::processed();
    let rpc_client = Arc::new(get_rpc_client(commitment));
    deploy_loader_v4(
        rpc_client.clone(),
        &prog_kp,
        &auth_kp,
        &program_data,
        false,
    )
    .await;

    debug!("Program deployed V4: {}", prog_kp.pubkey());
    assert_program_owned_by_loader!(&rpc_client, &prog_kp.pubkey(), 4);

    // 1. Ensure that the program works on the remote
    test_mini_program!(&rpc_client, &prog_kp.pubkey(), &auth_kp);

    // 2. Ensure we can directly deserialize the program account
    let loaded_program = fetch_and_assert_loaded_program_v1_v2_v4!(
        rpc_client,
        prog_kp.pubkey(),
        LoadedProgram {
            program_id: prog_kp.pubkey(),
            authority: auth_kp.pubkey(),
            program_data,
            loader: RemoteProgramLoader::V4,
            loader_status: LoaderV4Status::Deployed,
            remote_slot: 0
        }
    );

    // 3. Redeploy with v4 loader again and ensure it works
    let prog_kp = Keypair::new();
    let auth_kp = Keypair::new();
    let program_data = loaded_program.program_data;
    deploy_loader_v4(
        rpc_client.clone(),
        &prog_kp,
        &auth_kp,
        &program_data,
        false,
    )
    .await;
    test_mini_program_log_msg!(
        &rpc_client,
        &prog_kp.pubkey(),
        &auth_kp,
        "Hello new deployment"
    );

    // 4. Upload a shank IDL for the program
    let idl = b"Mini Program V4 IDL";
    mini_upload_idl!(
        &rpc_client,
        &auth_kp,
        &prog_kp.pubkey(),
        IdlType::Shank,
        idl
    );
}

// -----------------
// Fetching + cloning programs
// -----------------
#[tokio::test]
async fn ixtest_clone_memo_v1_loader_program() {
    init_logger();

    let ctx = IxtestContext::init().await;

    let pubkeys = [MEMOV1];

    ctx.chainlink.ensure_accounts(&pubkeys).await.unwrap();

    debug!("{}", ctx.cloner);
    assert_loaded_program_with_size!(
        ctx.cloner,
        &MEMOV1,
        &MEMOV1,
        RemoteProgramLoader::V1,
        LoaderV4Status::Finalized,
        17280
    );
    assert_subscribed_without_loaderv3_program_data_account!(
        ctx.chainlink,
        &pubkeys
    );
}

#[tokio::test]
async fn ixtest_clone_memo_v2_loader_program() {
    init_logger();

    let ctx = IxtestContext::init().await;

    let pubkeys = [MEMOV2];

    ctx.chainlink.ensure_accounts(&pubkeys).await.unwrap();

    debug!("{}", ctx.cloner);
    assert_loaded_program_with_size!(
        ctx.cloner,
        &MEMOV2,
        &MEMOV2,
        RemoteProgramLoader::V2,
        LoaderV4Status::Finalized,
        74800
    );
    assert_subscribed_without_loaderv3_program_data_account!(
        ctx.chainlink,
        &pubkeys
    );
}

const MINI_SIZE: usize = 91200;
#[tokio::test]
async fn ixtest_clone_mini_v2_loader_program() {
    init_logger();

    let ctx = IxtestContext::init().await;

    let pubkeys = [MINIV2];

    ctx.chainlink.ensure_accounts(&pubkeys).await.unwrap();

    debug!("{}", ctx.cloner);
    assert_loaded_program_with_size!(
        ctx.cloner,
        &MINIV2,
        &MINIV2,
        RemoteProgramLoader::V2,
        LoaderV4Status::Finalized,
        MINI_SIZE
    );
    assert_subscribed_without_loaderv3_program_data_account!(
        ctx.chainlink,
        &pubkeys
    );
}

#[tokio::test]
async fn ixtest_clone_mini_v3_loader_program() {
    init_logger();

    let ctx = IxtestContext::init().await;
    let pubkeys = [MINIV3];

    ctx.chainlink.ensure_accounts(&pubkeys).await.unwrap();

    debug!("{}", ctx.cloner);
    assert_loaded_program_with_size!(
        ctx.cloner,
        &MINIV3,
        &MINIV3_AUTH,
        RemoteProgramLoader::V3,
        LoaderV4Status::Deployed,
        MINI_SIZE
    );
    assert_subscribed_without_loaderv3_program_data_account!(
        ctx.chainlink,
        &pubkeys
    );
}

#[tokio::test]
async fn ixtest_clone_mini_v4_loader_program() {
    init_logger();

    let prog_kp = Keypair::new();
    let auth_kp = Keypair::new();

    let program_data = compile_mini(&prog_kp, None);
    debug!(
        "Binary size: {} ({})",
        pretty_bytes(program_data.len()),
        program_data.len()
    );

    let ctx = IxtestContext::init().await;
    deploy_loader_v4(
        ctx.rpc_client.clone(),
        &prog_kp,
        &auth_kp,
        &program_data,
        false,
    )
    .await;

    debug!("Program deployed V4: {}", prog_kp.pubkey());
    assert_program_owned_by_loader!(&ctx.rpc_client, &prog_kp.pubkey(), 4);

    // As mentioned above the v4 loader seems to pad with an extra 1KB
    const MINI_SIZE_V4: usize = MINI_SIZE + 1024;
    let pubkeys = [prog_kp.pubkey()];

    ctx.chainlink.ensure_accounts(&pubkeys).await.unwrap();

    debug!("{}", ctx.cloner);
    assert_loaded_program_with_size!(
        ctx.cloner,
        &prog_kp.pubkey(),
        &auth_kp.pubkey(),
        RemoteProgramLoader::V4,
        LoaderV4Status::Deployed,
        MINI_SIZE_V4
    );
    assert_subscribed_without_loaderv3_program_data_account!(
        ctx.chainlink,
        &pubkeys
    );
}

#[tokio::test]
async fn ixtest_clone_multiple_programs_v1_v2_v3() {
    init_logger();

    let ctx = IxtestContext::init().await;

    let pubkeys = [MEMOV1, MEMOV2, MINIV3];

    ctx.chainlink.ensure_accounts(&pubkeys).await.unwrap();

    debug!("{}", ctx.cloner);

    assert_loaded_program_with_size!(
        ctx.cloner,
        &MEMOV1,
        &MEMOV1,
        RemoteProgramLoader::V1,
        LoaderV4Status::Finalized,
        17280
    );
    assert_loaded_program_with_size!(
        ctx.cloner,
        &MEMOV2,
        &MEMOV2,
        RemoteProgramLoader::V2,
        LoaderV4Status::Finalized,
        74800
    );
    assert_loaded_program_with_size!(
        ctx.cloner,
        &MINIV3,
        &MINIV3_AUTH,
        RemoteProgramLoader::V3,
        LoaderV4Status::Deployed,
        MINI_SIZE
    );
    assert_subscribed_without_loaderv3_program_data_account!(
        ctx.chainlink,
        &pubkeys
    );
}
