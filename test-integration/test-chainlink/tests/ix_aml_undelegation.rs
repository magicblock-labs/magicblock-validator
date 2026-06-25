use std::{
    collections::HashMap,
    io::{Read, Write},
    net::TcpListener,
    sync::Arc,
    time::Duration,
};

use dlp_api::{
    args::{
        EncryptedBuffer, MaybeEncryptedAccountMeta, MaybeEncryptedInstruction,
        MaybeEncryptedIxData, PostDelegationActions,
    },
    pda::delegation_record_pda_from_delegated_account,
    state::DelegationRecord,
};
use magicblock_aml::RiskService;
use magicblock_chainlink::testing::init_logger;
use magicblock_config::config::RiskConfig;
use solana_account::Account;
use solana_pubkey::Pubkey;
use solana_sdk_ids::system_program;
use test_chainlink::test_context::TestContext;
use tokio::task::JoinHandle;

struct MockRiskServer {
    base_url: String,
    worker: JoinHandle<()>,
}

impl MockRiskServer {
    async fn start(
        address_scores: Vec<(String, u64)>,
        expected_calls: usize,
    ) -> Self {
        let listener =
            TcpListener::bind("127.0.0.1:0").expect("bind mock risk server");
        let addr = listener.local_addr().expect("mock risk server address");
        let score_by_address: HashMap<String, u64> =
            address_scores.into_iter().collect();

        let worker = tokio::task::spawn_blocking(move || {
            for _ in 0..expected_calls {
                let (mut stream, _) =
                    listener.accept().expect("accept mock risk request");
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .expect("set mock risk read timeout");

                let mut buffer = [0u8; 4096];
                let read = stream.read(&mut buffer).expect("read request");
                let request = String::from_utf8_lossy(&buffer[..read]);
                let address = extract_query_value(&request, "address")
                    .expect("missing address query");
                assert!(request.starts_with("GET /risk/address?"));
                assert!(request.contains("network=solana"));

                let score = score_by_address
                    .get(&address)
                    .expect("unexpected risk address");
                let body = format!(r#"{{"riskScore":{score}}}"#);
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream
                    .write_all(response.as_bytes())
                    .expect("write mock risk response");
            }
        });

        Self {
            base_url: format!("http://{addr}"),
            worker,
        }
    }

    async fn join(self) {
        self.worker.await.expect("mock risk server panicked");
    }
}

fn extract_query_value(request: &str, key: &str) -> Option<String> {
    let query = request
        .lines()
        .next()?
        .split_whitespace()
        .nth(1)?
        .split('?')
        .nth(1)?;
    query.split('&').find_map(|part| {
        let (k, v) = part.split_once('=')?;
        (k == key).then(|| v.to_string())
    })
}

fn risk_config(base_url: String) -> RiskConfig {
    RiskConfig {
        enabled: true,
        base_url,
        api_key: Some("test-api-key".to_string()),
        cache_ttl: Duration::from_secs(60),
        request_timeout: Duration::from_secs(2),
        risk_score_threshold: 7,
    }
}

fn add_delegation_record_with_signer_action(
    ctx: &TestContext,
    delegated_pubkey: Pubkey,
    owner: Pubkey,
    signer: Pubkey,
) {
    let record = DelegationRecord {
        authority: ctx.validator_pubkey,
        owner,
        delegation_slot: 1,
        lamports: 1_000,
        commit_frequency_ms: 2_000,
    };
    let mut data = vec![0; DelegationRecord::size_with_discriminator()];
    record.to_bytes_with_discriminator(&mut data).unwrap();

    let actions = PostDelegationActions {
        inserted_signers: 0,
        inserted_non_signers: 0,
        signers: vec![*signer.as_array(), *system_program::id().as_array()],
        non_signers: vec![],
        instructions: vec![MaybeEncryptedInstruction {
            program_id: 1,
            accounts: vec![MaybeEncryptedAccountMeta::ClearText(
                dlp_api::compact::AccountMeta::new_readonly(0, true),
            )],
            data: MaybeEncryptedIxData {
                prefix: vec![1],
                suffix: EncryptedBuffer::default(),
            },
        }],
    };
    data.extend_from_slice(&borsh::to_vec(&actions).unwrap());

    ctx.rpc_client.add_account(
        delegation_record_pda_from_delegated_account(&delegated_pubkey),
        Account {
            owner: dlp_api::id(),
            data,
            ..Default::default()
        },
    );
}

async fn setup(
    risk_score: u64,
) -> (TestContext, MockRiskServer, Pubkey, tempfile::TempDir) {
    init_logger();

    let signer = Pubkey::new_unique();
    let server =
        MockRiskServer::start(vec![(signer.to_string(), risk_score)], 1).await;
    // Keep alive for the test: RiskService writes risk-cache.db under this path.
    let temp_ledger = tempfile::tempdir().expect("temp ledger");
    let risk_service = RiskService::try_from_config(
        &risk_config(server.base_url.clone()),
        temp_ledger.path(),
    )
    .expect("risk config should be valid")
    .expect("risk service should be enabled");
    let risk_service = Arc::new(risk_service);

    let slot = 100;
    let ctx =
        TestContext::init_with_risk_service(slot, Some(risk_service)).await;

    let delegated_pubkey = Pubkey::new_unique();
    let owner = system_program::id();
    ctx.rpc_client.add_account(
        delegated_pubkey,
        Account {
            lamports: 1_000_000,
            data: vec![1, 2, 3, 4],
            owner: dlp_api::id(),
            executable: false,
            rent_epoch: 0,
        },
    );
    add_delegation_record_with_signer_action(
        &ctx,
        delegated_pubkey,
        owner,
        signer,
    );

    (ctx, server, delegated_pubkey, temp_ledger)
}

#[tokio::test]
async fn post_delegation_aml_rejection_schedules_undelegation_with_high_risk_signer(
) {
    let (ctx, server, delegated_pubkey, _temp_ledger) = setup(9).await;

    ctx.ensure_account(&delegated_pubkey)
        .await
        .expect("high-risk signer should still be cloned");

    let req = ctx.cloner.clone_requests().first().cloned().unwrap();
    assert!(
        req.needs_undelegation,
        "AML-rejected action must schedule undelegation"
    );

    server.join().await;
}

#[tokio::test]
async fn post_delegation_aml_accepts_low_risk_signer() {
    let (ctx, server, delegated_pubkey, _temp_ledger) = setup(1).await;

    ctx.ensure_account(&delegated_pubkey)
        .await
        .expect("low-risk signer should be cloned");

    let req = ctx.cloner.clone_requests().first().cloned().unwrap();
    assert!(
        !req.needs_undelegation,
        "AML-accepted action must not schedule undelegation"
    );

    server.join().await;
}
