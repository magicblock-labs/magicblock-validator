#![allow(unused)]

use log::*;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_rpc_client_api::{
    client_error::Result as ClientResult, config::RpcSendTransactionConfig,
};
use solana_sdk::{
    instruction::Instruction,
    native_token::LAMPORTS_PER_SOL,
    pubkey,
    signature::{Keypair, Signature},
    signer::Signer,
    transaction::Transaction,
};

/// The memo v1 program is predeployed with the v1 loader
/// (BPFLoader1111111111111111111111111111111111)
/// at this program ID in the test validator.
pub const MEMOV1: Pubkey =
    pubkey!("Memo1UhkJRfHyvLMcVucJwxXeuD728EqVDDwQDxFMNo");
/// The memo v2 program is predeployed with the v1 loader
/// (BPFLoader2111111111111111111111111111111111)
/// at this program ID in the test validator.
pub const MEMOV2: Pubkey =
    pubkey!("MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr");
/// Another v1 program that is predeployed with the v1 loader
/// (BPFLoader1111111111111111111111111111111111)
/// at this program ID in the test validator.
pub const OTHERV1: Pubkey =
    pubkey!("BL5oAaURQwAVVHcgrucxJe3H5K57kCQ5Q8ys7dctqfV8");
/// The mini program is predeployed with the v2 loader
/// (BPFLoader2111111111111111111111111111111111)
/// at this program ID in the test validator.
pub const MINIV2: Pubkey =
    pubkey!("MiniV21111111111111111111111111111111111111");
/// The mini program is predeployed with the v3 loader
/// (BPFLoaderUpgradeab1e11111111111111111111111)
/// at this program ID in the test validator.
pub const MINIV3: Pubkey =
    pubkey!("MiniV31111111111111111111111111111111111111");

/// The authority with which the mini program for v3 loader is deployed
pub const MINIV3_AUTH: Pubkey =
    pubkey!("MiniV3AUTH111111111111111111111111111111111");
/// The authority with which the mini program for v4 loader is deployed
/// NOTE: V4 is compiled and deployed during test setup using the
/// [deploy_loader_v4] method (LoaderV411111111111111111111111111111111111)
pub const MINIV4_AUTH: Pubkey =
    pubkey!("MiniV4AUTH111111111111111111111111111111111");

/// Additional v3 loader program for testing parallel cloning of multiple
/// programs. This program is cloned from devnet in ephemeral tests.
/// Note: In devnet tests, these programs are deployed with different IDs
/// but using the same MINIV3 binary. They're used to test batched fetching
/// of multiple LoaderV3 programs.
pub const PARALLEL_MINIV3_1: Pubkey =
    pubkey!("MiniV32111111111111111111111111111111111111");
pub const PARALLEL_MINIV3_1_AUTH: Pubkey =
    pubkey!("MiniV4AUTH211111111111111111111111111111111");

pub const PARALLEL_MINIV3_2: Pubkey =
    pubkey!("MiniV33111111111111111111111111111111111111");
pub const PARALLEL_MINIV3_2_AUTH: Pubkey =
    pubkey!("MiniV4AUTH311111111111111111111111111111111");

const CHUNK_SIZE: usize = 800;

pub async fn airdrop_sol(
    rpc_client: &RpcClient,
    pubkey: &solana_sdk::pubkey::Pubkey,
    sol: u64,
) {
    let airdrop_signature = rpc_client
        .request_airdrop(pubkey, sol * LAMPORTS_PER_SOL)
        .await
        .expect("Failed to request airdrop");

    rpc_client
        .confirm_transaction(&airdrop_signature)
        .await
        .expect("Failed to confirm airdrop");

    debug!("Airdropped {sol} SOL to account {pubkey}");
}

async fn send_transaction(
    rpc_client: &RpcClient,
    transaction: &Transaction,
    label: &str,
) -> Signature {
    rpc_client
        .send_and_confirm_transaction_with_spinner_and_config(
            transaction,
            rpc_client.commitment(),
            RpcSendTransactionConfig {
                skip_preflight: true,
                ..Default::default()
            },
        )
        .await
        .inspect_err(|err| {
            error!("{label} encountered error:{err:#?}");
            info!("Signature: {}", transaction.signatures[0]);
        })
        .expect("Failed to send and confirm transaction")
}

pub async fn send_instructions(
    rpc_client: &RpcClient,
    ixs: &[Instruction],
    signers: &[&Keypair],
    label: &str,
) -> Signature {
    let recent_blockhash = rpc_client
        .get_latest_blockhash()
        .await
        .expect("Failed to get recent blockhash");
    let mut transaction =
        Transaction::new_with_payer(ixs, Some(&signers[0].pubkey()));
    transaction.sign(signers, recent_blockhash);
    send_transaction(rpc_client, &transaction, label).await
}

async fn try_send_transaction(
    rpc_client: &RpcClient,
    transaction: &Transaction,
    label: &str,
) -> ClientResult<Signature> {
    rpc_client
        .send_and_confirm_transaction_with_spinner_and_config(
            transaction,
            rpc_client.commitment(),
            RpcSendTransactionConfig {
                skip_preflight: true,
                ..Default::default()
            },
        )
        .await
        .inspect_err(|err| {
            error!("{label} encountered error:{err:#?}");
            info!("Signature: {}", transaction.signatures[0]);
        })
}

pub async fn try_send_instructions(
    rpc_client: &RpcClient,
    ixs: &[Instruction],
    signers: &[&Keypair],
    label: &str,
) -> ClientResult<Signature> {
    let recent_blockhash = rpc_client
        .get_latest_blockhash()
        .await
        .expect("Failed to get recent blockhash");
    let mut transaction =
        Transaction::new_with_payer(ixs, Some(&signers[0].pubkey()));
    transaction.sign(signers, recent_blockhash);
    try_send_transaction(rpc_client, &transaction, label).await
}

pub mod resolve_deploy {
    #[macro_export]
    macro_rules! fetch_and_assert_loaded_program_v1_v2_v4 {
        ($rpc_client:expr, $program_id:expr, $expected:expr) => {{
            use log::*;
            use solana_loader_v4_interface::state::LoaderV4Status;
            use solana_sdk::account::AccountSharedData;

            let program_account = $rpc_client
                .get_account(&$program_id)
                .await
                .expect("Failed to get program account");
            let resolver = ProgramAccountResolver::try_new(
                $program_id,
                program_account.owner,
                Some(AccountSharedData::from(program_account.clone())),
                None,
            )
            .expect("Failed to resolve program account");

            let mut loaded_program = resolver.into_loaded_program();
            debug!("Loaded program: {loaded_program}");

            let mut expected = $expected;

            // NOTE: it seems that the v4 loader pads the deployed program
            // with zeros thus that it is a bit larger than the original
            // I verified with the explorere that it is actually present in the
            // validator with that increased size.
            let len = expected.program_data.len();
            loaded_program.program_data.truncate(len);
            // We don't care about the remote slot here, so we just make sure it
            // matches so the assert_eq below works
            expected.remote_slot = loaded_program.remote_slot;

            debug!("Expected program: {expected}");
            assert_eq!(loaded_program, expected);

            loaded_program
        }};
    }

    #[macro_export]
    macro_rules! fetch_and_assert_loaded_program_v3 {
        ($rpc_client:expr, $program_id:expr, $expected:expr) => {{
            use magicblock_chainlink::remote_account_provider::program_account::{
                get_loaderv3_get_program_data_address, ProgramAccountResolver,
            };
            let program_data_addr =
                get_loaderv3_get_program_data_address(&$program_id);
            let program_account = $rpc_client
                .get_account(&$program_id)
                .await
                .expect("Failed to get program account");
            let program_data_account = $rpc_client
                .get_account(&program_data_addr)
                .await
                .expect("Failed to get program account");
            let resolver = ProgramAccountResolver::try_new(
                $program_id,
                program_account.owner,
                None,
                Some(solana_account::AccountSharedData::from(
                    program_data_account,
                )),
            )
            .expect("Failed to create program account resolver");

            let loaded_program = resolver.into_loaded_program();
            debug!("Loaded program: {loaded_program}");

            let mut expected = $expected;
            // We don't care about the remote slot here, so we just make sure it
            // matches so the assert_eq below works
            expected.remote_slot = loaded_program.remote_slot;

            assert_eq!(loaded_program, expected);

            loaded_program
        }};
    }
}

pub mod memo {
    use solana_pubkey::Pubkey;
    use solana_sdk::instruction::{AccountMeta, Instruction};

    /// Memo instruction copied here in order to work around the stupid
    /// Address vs Pubkey issue (thanks anza) + not needing spl-memo-interface crate
    pub fn build_memo(
        program_id: &Pubkey,
        memo: &[u8],
        signer_pubkeys: &[&Pubkey],
    ) -> Instruction {
        Instruction {
            program_id: *program_id,
            accounts: signer_pubkeys
                .iter()
                .map(|&pubkey| AccountMeta::new_readonly(*pubkey, true))
                .collect(),
            data: memo.to_vec(),
        }
    }
}

#[allow(unused)]
pub mod mini {
    use program_mini::{common::IdlType, sdk};
    use solana_pubkey::Pubkey;
    use solana_rpc_client::nonblocking::rpc_client::RpcClient;
    use solana_sdk::{
        signature::{Keypair, Signature},
        signer::Signer,
    };

    use super::send_instructions;

    // -----------------
    // Binaries
    // -----------------
    pub(super) fn program_path(version: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("target")
            .join("deploy")
            .join(version)
            .join("program_mini.so")
    }

    pub fn load_miniv2_so() -> Vec<u8> {
        std::fs::read(program_path("miniv2"))
            .expect("Failed to read program_mini.so")
    }

    pub fn load_miniv3_so() -> Vec<u8> {
        std::fs::read(program_path("miniv3"))
            .expect("Failed to read program_mini.so")
    }

    // -----------------
    // IDL
    // -----------------
    pub async fn send_and_confirm_upload_idl_transaction(
        rpc_client: &RpcClient,
        auth_kp: &Keypair,
        program_id: &Pubkey,
        idl_type: IdlType,
        idl: &[u8],
    ) -> Signature {
        use IdlType::*;
        let sdk = sdk::MiniSdk::new(*program_id);
        let ix = match idl_type {
            Anchor => sdk.add_anchor_idl_instruction(&auth_kp.pubkey(), idl),
            Shank => sdk.add_shank_idl_instruction(&auth_kp.pubkey(), idl),
        };

        send_instructions(rpc_client, &[ix], &[auth_kp], "upload_idl").await
    }

    pub async fn get_idl(
        rpc_client: &RpcClient,
        program_id: &Pubkey,
        idl_type: IdlType,
    ) -> Option<Vec<u8>> {
        use IdlType::*;
        let sdk = sdk::MiniSdk::new(*program_id);
        let idl_pda = match idl_type {
            Anchor => sdk.anchor_idl_pda(),
            Shank => sdk.shank_idl_pda(),
        };

        let account = rpc_client
            .get_account(&idl_pda.0)
            .await
            .expect("IDL account not found");

        if account.data.is_empty() {
            None
        } else {
            Some(account.data)
        }
    }

    #[macro_export]
    macro_rules! mini_upload_idl {
        ($rpc_client:expr, $auth_kp:expr, $program_id:expr, $idl_type:expr, $idl:expr) => {{
            use $crate::programs::mini::send_and_confirm_upload_idl_transaction;
            let sig = send_and_confirm_upload_idl_transaction(
                $rpc_client,
                $auth_kp,
                $program_id,
                $idl_type,
                $idl,
            )
            .await;
            let uploaded_idl = $crate::programs::mini::get_idl(
                $rpc_client,
                $program_id,
                $idl_type,
            )
            .await;
            assert!(uploaded_idl.is_some(), "Uploaded IDL should not be None");
            debug!(
                "Uploaded {} IDL: '{}' via {sig}",
                stringify!($idl_type),
                String::from_utf8_lossy(&uploaded_idl.as_ref().unwrap())
            );
            assert_eq!(
                uploaded_idl.as_ref().unwrap(),
                $idl,
                "Uploaded IDL does not match expected IDL"
            );
        }};
    }

    // -----------------
    // Init
    // -----------------
    pub async fn send_and_confirm_init_transaction(
        rpc_client: &RpcClient,
        program_id: &Pubkey,
        auth_kp: &Keypair,
    ) -> Signature {
        let sdk = sdk::MiniSdk::new(*program_id);
        let init_ix = sdk.init_instruction(&auth_kp.pubkey());
        send_instructions(rpc_client, &[init_ix], &[auth_kp], "counter:init")
            .await
    }

    pub async fn send_and_confirm_increment_transaction(
        rpc_client: &RpcClient,
        program_id: &Pubkey,
        auth_kp: &Keypair,
    ) -> Signature {
        let sdk = sdk::MiniSdk::new(*program_id);
        let increment_ix = sdk.increment_instruction(&auth_kp.pubkey());
        send_instructions(
            rpc_client,
            &[increment_ix],
            &[auth_kp],
            "counter:inc",
        )
        .await
    }

    pub async fn send_and_confirm_log_msg_transaction(
        rpc_client: &RpcClient,
        program_id: &Pubkey,
        auth_kp: &Keypair,
        msg: &str,
    ) -> Signature {
        let sdk = sdk::MiniSdk::new(*program_id);
        let log_msg_ix = sdk.log_msg_instruction(&auth_kp.pubkey(), msg);
        send_instructions(
            rpc_client,
            &[log_msg_ix],
            &[auth_kp],
            "counter:log_msg",
        )
        .await
    }

    pub async fn get_counter(
        rpc_client: &RpcClient,
        program_id: &Pubkey,
        auth_kp: &Keypair,
    ) -> u64 {
        let counter_pda =
            sdk::MiniSdk::new(*program_id).counter_pda(&auth_kp.pubkey());
        let account = rpc_client
            .get_account(&counter_pda.0)
            .await
            .expect("Counter account not found");

        // Deserialize the counter value from the account data
        u64::from_le_bytes(
            account.data[0..8]
                .try_into()
                .expect("Invalid counter data length"),
        )
    }

    #[macro_export]
    macro_rules! assert_program_owned_by_loader {
        ($rpc_client:expr, $program_id:expr, $loader_version:expr) => {{
            use solana_pubkey::pubkey;
            let loader_id = match $loader_version {
                1 => pubkey!("BPFLoader1111111111111111111111111111111111"),
                2 => pubkey!("BPFLoader2111111111111111111111111111111111"),
                3 => pubkey!("BPFLoaderUpgradeab1e11111111111111111111111"),
                4 => pubkey!("LoaderV411111111111111111111111111111111111"),
                _ => panic!("Unsupported loader version: {}", $loader_version),
            };
            let program_account = $rpc_client
                .get_account($program_id)
                .await
                .expect("Failed to get program account");

            assert_eq!(
                program_account.owner, loader_id,
                "Program {} is not owned by loader {}, but by {}",
                $program_id, loader_id, program_account.owner
            );
        }};
    }

    #[macro_export]
    macro_rules! test_mini_program {
        ($rpc_client:expr, $program_id:expr, $auth_kp:expr) => {{
            use log::*;
            // Initialize the counter
            let init_signature =
                $crate::programs::mini::send_and_confirm_init_transaction(
                    $rpc_client,
                    $program_id,
                    $auth_kp,
                )
                .await;

            debug!("Initialized counter with signature {}", init_signature);
            let counter_value = $crate::programs::mini::get_counter(
                $rpc_client,
                $program_id,
                $auth_kp,
            )
            .await;
            assert_eq!(counter_value, 0, "Counter should be initialized to 0");
            debug!("Counter value after init: {}", counter_value);

            // Increment the counter
            let increment_signature =
                $crate::programs::mini::send_and_confirm_increment_transaction(
                    $rpc_client,
                    $program_id,
                    $auth_kp,
                )
                .await;
            debug!(
                "Incremented counter with signature {}",
                increment_signature
            );
            let counter_value = $crate::programs::mini::get_counter(
                $rpc_client,
                $program_id,
                $auth_kp,
            )
            .await;
            debug!("Counter value after first increment: {}", counter_value);
            assert_eq!(
                counter_value, 1,
                "Counter should be 1 after first increment"
            );

            // Increment the counter again
            let increment_signature =
                $crate::programs::mini::send_and_confirm_increment_transaction(
                    $rpc_client,
                    $program_id,
                    $auth_kp,
                )
                .await;
            debug!(
                "Incremented counter again with signature {}",
                increment_signature
            );
            let counter_value = $crate::programs::mini::get_counter(
                $rpc_client,
                $program_id,
                $auth_kp,
            )
            .await;
            debug!("Counter value after second increment: {}", counter_value);
            assert_eq!(
                counter_value, 2,
                "Counter should be 2 after second increment"
            );
        }};
    }
    /// NOTE: use this for redeploys at a different program id.
    /// This instruction does not depend on them matching as the others do.
    #[macro_export]
    macro_rules! test_mini_program_log_msg {
        ($rpc_client:expr, $program_id:expr, $auth_kp:expr, $msg:expr) => {{
            use log::*;
            let log_msg_signature =
                $crate::programs::mini::send_and_confirm_log_msg_transaction(
                    $rpc_client,
                    $program_id,
                    $auth_kp,
                    $msg,
                )
                .await;
            debug!("Sent log message with signature {}", log_msg_signature);
        }};
    }
}

#[allow(unused)]
pub mod deploy {
    use std::{fs, path::PathBuf, process::Command, sync::Arc};

    use log::*;
    use solana_loader_v4_interface::instruction::LoaderV4Instruction as LoaderInstructionV4;
    use solana_rpc_client::nonblocking::rpc_client::RpcClient;
    use solana_sdk::{
        instruction::{AccountMeta, Instruction},
        loader_v4, loader_v4_instruction,
        native_token::LAMPORTS_PER_SOL,
        signature::Keypair,
        signer::Signer,
    };
    use solana_system_interface::instruction as system_instruction;

    use super::{airdrop_sol, send_instructions, CHUNK_SIZE};
    use crate::programs::{mini, try_send_instructions};

    pub fn compile_mini(keypair: &Keypair, suffix: Option<&str>) -> Vec<u8> {
        let workspace_root_path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
        let program_root_path =
            workspace_root_path.join("programs").join("mini");
        let program_id = keypair.pubkey().to_string();

        // Build the program and read the binary, ensuring cleanup happens
        // Run cargo build-sbf to compile the program
        let mut cmd = Command::new("cargo");
        if let Some(suffix) = suffix {
            cmd.env("LOG_MSG_SUFFIX", suffix);
        }
        let output = cmd
            .env("MINI_PROGRAM_ID", &program_id)
            .args([
                "build-sbf",
                "--manifest-path",
                program_root_path.join("Cargo.toml").to_str().unwrap(),
                "--sbf-out-dir",
                mini::program_path("miniv4")
                    .parent()
                    .unwrap()
                    .to_str()
                    .unwrap(),
            ])
            .output()
            .expect("Failed to run cargo build-sbf");

        if !output.status.success() {
            panic!(
                "cargo build-sbf failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        // Read the compiled binary (typically in target/deploy/*.so)
        let binary_path = mini::program_path("miniv4");
        fs::read(binary_path).expect("Failed to read compiled program binary")
    }

    pub async fn deploy_loader_v4(
        rpc_client: Arc<RpcClient>,
        program_kp: &Keypair,
        auth_kp: &Keypair,
        program_data: &[u8],
        deploy_should_fail: bool,
    ) {
        // Airdrop SOL to auth keypair for transaction fees
        airdrop_sol(&rpc_client, &auth_kp.pubkey(), 20).await;

        // BPF Loader v4 program ID
        let loader_program_id =
            solana_sdk::pubkey!("LoaderV411111111111111111111111111111111111");

        // 1. Set program length to initialize and allocate space
        if rpc_client.get_account(&program_kp.pubkey()).await.is_err() {
            let create_program_account_instruction =
                system_instruction::create_account(
                    &auth_kp.pubkey(),
                    &program_kp.pubkey(),
                    10 * LAMPORTS_PER_SOL,
                    0,
                    &loader_program_id,
                );
            let signature = send_instructions(
                &rpc_client,
                &[create_program_account_instruction],
                &[auth_kp, program_kp],
                "deploy_loader_v4::create_program_account_instruction",
            )
            .await;
            debug!("Created program account: {signature}");
        } else {
            let retract_instruction =
                loader_v4::retract(&program_kp.pubkey(), &auth_kp.pubkey());
            let signature = send_instructions(
                &rpc_client,
                &[retract_instruction],
                &[auth_kp],
                "deploy_loader_v4::create_program_account_instruction",
            )
            .await;
            debug!("Retracted program account: {signature}");
        }

        let set_length_instruction = {
            let loader_instruction = LoaderInstructionV4::SetProgramLength {
                new_size: program_data.len() as u32 + 1024,
            };

            Instruction {
                program_id: loader_program_id,
                accounts: vec![
                    // [writable] The program account to change the size of
                    AccountMeta::new(program_kp.pubkey(), false),
                    // [signer] The authority of the program
                    AccountMeta::new_readonly(auth_kp.pubkey(), true),
                ],
                data: bincode::serialize(&loader_instruction)
                    .expect("Failed to serialize SetProgramLength instruction"),
            }
        };

        let signature = send_instructions(
            &rpc_client,
            &[set_length_instruction],
            &[auth_kp],
            "deploy_loader_v4::set_length_instruction",
        )
        .await;

        debug!("Initialized length: {signature}");

        // 2. Write program data
        use futures::stream::{self, StreamExt};

        const MAX_CONCURRENCY: usize = 100;

        let tasks =
            program_data
                .chunks(CHUNK_SIZE)
                .enumerate()
                .map(|(idx, chunk)| {
                    let chunk = chunk.to_vec();
                    let offset = (idx * CHUNK_SIZE) as u32;
                    let program_pubkey = program_kp.pubkey();
                    let auth_kp = auth_kp.insecure_clone();
                    let auth_pubkey = auth_kp.pubkey();
                    let rpc_client = rpc_client.clone();

                    async move {
                        let chunk_size = chunk.len();
                        let loader_instruction = LoaderInstructionV4::Write {
                            offset,
                            bytes: chunk,
                        };

                        let instruction = Instruction {
                            program_id: loader_program_id,
                            accounts: vec![
                                AccountMeta::new(program_pubkey, false),
                                AccountMeta::new_readonly(auth_pubkey, true),
                            ],
                            data: bincode::serialize(&loader_instruction)
                                .expect(
                                    "Failed to serialize Write instruction",
                                ),
                        };

                        let signature = send_instructions(
                            &rpc_client,
                            &[instruction],
                            &[&auth_kp],
                            "deploy_loader_v4::write_instruction",
                        )
                        .await;
                        trace!(
                        "Wrote chunk {idx} of size {chunk_size}: {signature}"
                    );
                        signature
                    }
                });

        let results: Vec<_> = stream::iter(tasks)
            .buffer_unordered(MAX_CONCURRENCY)
            .collect()
            .await;

        // 3. Deploy the program to make it executable
        let deploy_instruction = {
            let loader_instruction = LoaderInstructionV4::Deploy;

            Instruction {
                program_id: loader_program_id,
                accounts: vec![
                    // [writable] The program account to deploy
                    AccountMeta::new(program_kp.pubkey(), false),
                    // [signer] The authority of the program
                    AccountMeta::new_readonly(auth_kp.pubkey(), true),
                ],
                data: bincode::serialize(&loader_instruction)
                    .expect("Failed to serialize Deploy instruction"),
            }
        };

        if deploy_should_fail {
            let result = try_send_instructions(
                &rpc_client,
                &[deploy_instruction],
                &[auth_kp],
                "deploy_loader_v4::deploy_instruction",
            )
            .await;
            assert!(
                result.is_err(),
                "Deployment was expected to fail but succeeded"
            );
            debug!(
                "Deployment failed as expected with error: {:?}",
                result.err().unwrap()
            );
        } else {
            let signature = send_instructions(
                &rpc_client,
                &[deploy_instruction],
                &[auth_kp],
                "deploy_loader_v4::deploy_instruction",
            )
            .await;

            info!(
                "Deployed V4 program {} with signature {}",
                program_kp.pubkey(),
                signature
            );
        }
    }
}

// -----------------
// Not working
// -----------------
#[allow(unused)]
pub mod not_working {
    use std::sync::Arc;

    use log::*;
    use magicblock_chainlink::remote_account_provider::program_account::get_loaderv3_get_program_data_address;
    use solana_loader_v2_interface::LoaderInstruction as LoaderInstructionV2;
    use solana_loader_v3_interface::instruction::UpgradeableLoaderInstruction as LoaderInstructionV3;
    use solana_rpc_client::nonblocking::rpc_client::RpcClient;
    use solana_rpc_client_api::config::RpcSendTransactionConfig;
    use solana_sdk::{
        instruction::{AccountMeta, Instruction},
        signature::Keypair,
        signer::Signer,
        transaction::Transaction,
    };
    use solana_system_interface::instruction as system_instruction;

    use super::{airdrop_sol, send_transaction, CHUNK_SIZE};
    pub async fn deploy_loader_v1(
        _rpc_client: &RpcClient,
        _program_kp: &Keypair,
        _auth_kp: &Keypair,
        _program_data: &[u8],
    ) {
        todo!("Implement V1 Loader deployment logic");
    }

    // NOTE: these would work if solana would allow it, but we get the following error:
    // > BPF loader management instructions are no longer supported

    pub async fn deploy_loader_v2(
        rpc_client: &RpcClient,
        program_kp: &Keypair,
        auth_kp: &Keypair,
        program_data: &[u8],
    ) {
        // Airdrop SOL to auth keypair for transaction fees
        airdrop_sol(rpc_client, &auth_kp.pubkey(), 20).await;

        // BPF Loader v2 program ID
        let loader_program_id =
            solana_sdk::pubkey!("BPFLoader2111111111111111111111111111111111");

        // 1. Write program data in chunks
        for (idx, chunk) in program_data.chunks(CHUNK_SIZE).enumerate() {
            // Create Write instruction to write program data in chunks
            let write_instruction = {
                let loader_instruction = LoaderInstructionV2::Write {
                    offset: (idx * CHUNK_SIZE) as u32,
                    bytes: chunk.to_vec(),
                };

                Instruction {
                    program_id: loader_program_id,
                    accounts: vec![
                        // [WRITE, SIGNER] Account to write to
                        solana_sdk::instruction::AccountMeta::new(
                            program_kp.pubkey(),
                            true,
                        ),
                    ],
                    data: bincode::serialize(&loader_instruction)
                        .expect("Failed to serialize Write instruction"),
                }
            };

            // Create transaction with the write instruction
            let recent_blockhash = rpc_client
                .get_latest_blockhash()
                .await
                .expect("Failed to get recent blockhash");

            let mut transaction = Transaction::new_with_payer(
                &[write_instruction],
                Some(&auth_kp.pubkey()),
            );

            // Sign transaction
            transaction.sign(&[auth_kp, program_kp], recent_blockhash);

            // Send transaction and confirm
            let signature = rpc_client
                .send_and_confirm_transaction_with_spinner_and_config(
                    &transaction,
                    rpc_client.commitment(),
                    RpcSendTransactionConfig {
                        skip_preflight: true,
                        ..Default::default()
                    },
                )
                .await
                .inspect_err(|err| {
                    error!("{err:#?}");
                    info!("Signature: {}", transaction.signatures[0]);
                })
                .expect("Failed to send and confirm transaction");

            trace!(
                "Wrote chunk {idx} of size {} with signature {signature}",
                chunk.len(),
            );
        }

        // 2. Create Finalize instruction
        let finalize_instruction = {
            let loader_instruction = LoaderInstructionV2::Finalize;

            Instruction {
                program_id: loader_program_id,
                accounts: vec![
                    // [WRITE, SIGNER] Account to finalize
                    AccountMeta::new(program_kp.pubkey(), true),
                    // [] Rent sysvar
                    AccountMeta::new_readonly(
                        solana_sdk::sysvar::rent::id(),
                        false,
                    ),
                ],
                data: bincode::serialize(&loader_instruction)
                    .expect("Failed to serialize Finalize instruction"),
            }
        };

        // Create transaction with both instructions
        let recent_blockhash = rpc_client
            .get_latest_blockhash()
            .await
            .expect("Failed to get recent blockhash");

        let mut transaction = Transaction::new_with_payer(
            &[finalize_instruction],
            Some(&auth_kp.pubkey()),
        );

        // Sign transaction
        transaction.sign(&[auth_kp, program_kp], recent_blockhash);

        // Send transaction and confirm
        let signature = rpc_client
            .send_and_confirm_transaction(&transaction)
            .await
            .expect("Failed to send and confirm transaction");

        info!(
            "Deployed program {} with signature {}",
            program_kp.pubkey(),
            signature
        );
    }

    pub async fn deploy_loader_v3(
        rpc_client: &Arc<RpcClient>,
        program_kp: &Keypair,
        auth_kp: &Keypair,
        program_data: &[u8],
    ) {
        // Airdrop SOL to auth keypair for transaction fees
        airdrop_sol(rpc_client, &auth_kp.pubkey(), 2).await;
        // BPF Loader v3 (Upgradeable) program ID
        let loader_program_id =
            solana_sdk::pubkey!("BPFLoaderUpgradeab1e11111111111111111111111");

        // Generate buffer account
        let buffer_kp = Keypair::new();

        // Derive program data account address
        let program_data_address =
            get_loaderv3_get_program_data_address(&program_kp.pubkey());

        // Calculate required space for buffer account (program data + metadata)
        let buffer_space = program_data.len() + 37;
        let rent_exemption = rpc_client
            .get_minimum_balance_for_rent_exemption(buffer_space)
            .await
            .expect("Failed to get rent exemption");
        let recent_blockhash = rpc_client
            .get_latest_blockhash()
            .await
            .expect("Failed to get recent blockhash");

        // 1. Create and Initialize Buffer
        let create_buffer_instruction = system_instruction::create_account(
            &auth_kp.pubkey(),
            &buffer_kp.pubkey(),
            rent_exemption,
            buffer_space as u64,
            &loader_program_id,
        );
        debug!(
            "Creating buffer account {} with space {} and rent exemption {}",
            buffer_kp.pubkey(),
            buffer_space,
            rent_exemption
        );

        let init_buffer_instruction = {
            let loader_instruction = LoaderInstructionV3::InitializeBuffer;

            Instruction {
                program_id: loader_program_id,
                accounts: vec![
                    // [writable] Buffer account to initialize
                    AccountMeta::new(buffer_kp.pubkey(), false),
                    // [] Buffer authority (optional)
                    AccountMeta::new_readonly(auth_kp.pubkey(), false),
                ],
                data: bincode::serialize(&loader_instruction)
                    .expect("Failed to serialize InitializeBuffer instruction"),
            }
        };

        let mut transaction = Transaction::new_with_payer(
            &[create_buffer_instruction, init_buffer_instruction],
            Some(&auth_kp.pubkey()),
        );

        // Sign transaction
        transaction.sign(&[auth_kp, &buffer_kp], recent_blockhash);

        // Send transaction and confirm
        let signature =
            send_transaction(rpc_client, &transaction, "deploy_loaderv3::init")
                .await;

        debug!(
            "Created and initialized buffer {} with signature {}",
            buffer_kp.pubkey(),
            signature
        );

        // 2. Write program data to buffer
        let mut joinset = tokio::task::JoinSet::new();
        for (idx, chunk) in program_data.chunks(CHUNK_SIZE).enumerate() {
            let chunk = chunk.to_vec();
            let offset = (idx * CHUNK_SIZE) as u32;
            let buffer_pubkey = buffer_kp.pubkey();
            let auth_kp = auth_kp.insecure_clone();
            let auth_pubkey = auth_kp.pubkey();
            let rpc_client = rpc_client.clone();

            joinset.spawn(async move {
                let chunk_size = chunk.len();
                // Create Write instruction to write program data in chunks
                let loader_instruction = LoaderInstructionV3::Write {
                    offset,
                    bytes: chunk,
                };

                let instruction = Instruction {
                    program_id: loader_program_id,
                    accounts: vec![
                        // [writable] Buffer account to write to
                        AccountMeta::new(buffer_pubkey, false),
                        // [signer] Buffer authority
                        AccountMeta::new_readonly(auth_pubkey, true),
                    ],
                    data: bincode::serialize(&loader_instruction)
                        .expect("Failed to serialize Write instruction"),
                };

                let recent_blockhash = rpc_client
                    .get_latest_blockhash()
                    .await
                    .expect("Failed to get recent blockhash");

                let mut transaction = Transaction::new_with_payer(
                    &[instruction],
                    Some(&auth_pubkey),
                );

                // Sign transaction
                transaction.sign(&[&auth_kp], recent_blockhash);

                let signature = send_transaction(
                    &rpc_client,
                    &transaction,
                    "deploy_loaderv3::write",
                )
                .await;
                trace!("Wrote chunk {idx} of size {chunk_size}: {signature}");
                signature
            });
        }
        let _signatures = joinset.join_all().await;

        // 3. Deploy with max data length
        let deploy_instruction = {
            let loader_instruction =
                LoaderInstructionV3::DeployWithMaxDataLen {
                    max_data_len: program_data.len(),
                };

            Instruction {
                program_id: loader_program_id,
                accounts: vec![
                    // [writable, signer] The payer account
                    AccountMeta::new(auth_kp.pubkey(), true),
                    // [writable] The uninitialized ProgramData account
                    AccountMeta::new(program_data_address, false),
                    // [writable] The uninitialized Program account
                    AccountMeta::new(program_kp.pubkey(), false),
                    // [writable] The Buffer account with program data
                    AccountMeta::new(buffer_kp.pubkey(), false),
                    // [] Rent sysvar
                    AccountMeta::new_readonly(
                        solana_sdk::sysvar::rent::id(),
                        false,
                    ),
                    // [] Clock sysvar
                    AccountMeta::new_readonly(
                        solana_sdk::sysvar::clock::id(),
                        false,
                    ),
                    // [] System program
                    AccountMeta::new_readonly(
                        solana_sdk::system_program::id(),
                        false,
                    ),
                    // [signer] The program's authority
                    AccountMeta::new_readonly(auth_kp.pubkey(), true),
                ],
                data: bincode::serialize(&loader_instruction).expect(
                    "Failed to serialize DeployWithMaxDataLen instruction",
                ),
            }
        };

        let mut transaction = Transaction::new_with_payer(
            &[deploy_instruction],
            Some(&auth_kp.pubkey()),
        );

        // Sign transaction
        transaction.sign(&[auth_kp], recent_blockhash);

        // Send transaction and confirm
        let signature = send_transaction(
            rpc_client,
            &transaction,
            "deploy_loaderv3::deploy",
        )
        .await;

        info!(
            "Deployed V3 program {} with signature {}",
            program_kp.pubkey(),
            signature
        );
    }
}
