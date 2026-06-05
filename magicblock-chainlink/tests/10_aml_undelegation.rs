use std::{
    collections::HashMap,
    io::{Read, Write},
    net::TcpListener,
    sync::{Arc, Mutex},
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
use magicblock_chainlink::{
    chainlink::errors::ChainlinkResult,
    fetch_cloner::{UndelegationScheduleRequest, UndelegationScheduler},
    testing::init_logger,
};
use magicblock_config::config::RiskConfig;
use solana_account::{Account, ReadableAccount};
use solana_pubkey::Pubkey;
use solana_sdk_ids::system_program;
use tokio::task::JoinHandle;
use utils::test_context::TestContext;

mod utils;

#[derive(Default)]
struct RecordingUndelegationScheduler {
    requests: Mutex<Vec<UndelegationScheduleRequest>>,
}

impl RecordingUndelegationScheduler {
    fn requests(&self) -> Vec<UndelegationScheduleRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl UndelegationScheduler for RecordingUndelegationScheduler {
    async fn schedule_undelegation(
        &self,
        request: UndelegationScheduleRequest,
    ) -> ChainlinkResult<()> {
        self.requests.lock().unwrap().push(request);
        Ok(())
    }
}

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

#[tokio::test]
async fn post_delegation_aml_rejection_schedules_undelegation() {
    init_logger();

    let high_risk_signer = Pubkey::new_unique();
    let server =
        MockRiskServer::start(vec![(high_risk_signer.to_string(), 9)], 1).await;
    let temp_ledger = tempfile::tempdir().expect("temp ledger");
    let risk_service = RiskService::try_from_config(
        &risk_config(server.base_url.clone()),
        temp_ledger.path(),
    )
    .expect("risk config should be valid")
    .expect("risk service should be enabled");
    let risk_service = Arc::new(risk_service);
    let scheduler = Arc::new(RecordingUndelegationScheduler::default());

    let slot = 100;
    let ctx = TestContext::init_with_services(
        slot,
        Some(risk_service),
        Some(scheduler.clone() as Arc<dyn UndelegationScheduler>),
    )
    .await;

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
        high_risk_signer,
    );

    let err = ctx
        .ensure_account(&delegated_pubkey)
        .await
        .expect_err("high-risk signer should reject the clone");

    assert!(
        matches!(
            &err,
            magicblock_chainlink::errors::ChainlinkError::PendingRequestOwnerFailed(
                pubkey,
                message,
            ) if *pubkey == delegated_pubkey && message.contains("high risk")
        ),
        "unexpected error: {err:?}"
    );
    assert!(
        ctx.cloner.clone_requests().is_empty(),
        "AML-rejected action target must not be cloned"
    );

    let scheduled = scheduler.requests();
    assert_eq!(scheduled.len(), 1);
    assert_eq!(scheduled[0].pubkey, delegated_pubkey);
    assert!(scheduled[0].account.delegated());
    assert_eq!(scheduled[0].account.owner(), &owner);

    server.join().await;
}
