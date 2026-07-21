use std::{
    collections::HashMap,
    io::{self, Read, Write},
    net::TcpListener,
    path::Path,
    process::Child,
    str::FromStr,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, RwLock,
    },
    thread::{self, sleep},
    time::Duration,
};

use integration_test_tools::{
    expect,
    loaded_accounts::{LoadedAccounts, DLP_TEST_AUTHORITY_BYTES},
    validator::{
        cleanup, resolve_programs,
        start_magicblock_validator_with_config_struct,
    },
    IntegrationTestContext,
};
use magicblock_config::{
    config::{
        AccountsDbConfig, ChainLinkConfig, LedgerConfig, LifecycleMode,
        LoadableProgram, RiskConfig,
    },
    types::{Remote, StorageDirectory},
    ValidatorParams,
};
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, pubkey::Pubkey, signature::Keypair,
    signer::Signer, transaction::Transaction,
};
use tempfile::TempDir;

/// Threshold the mock risk server uses to decide `isRisky`. The test owner
/// risk scores (1 and 9) sit either side of it.
const MOCK_RISK_THRESHOLD: u64 = 5;

/// Mock of the risk server the validator queries. Serves the risk server's
/// `GET /risk?pubkey=` endpoint, computing `isRisky` from seeded scores.
pub struct MockRangeServer {
    base_url: String,
    request_count: Arc<AtomicUsize>,
    shutdown: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
    risks: Arc<RwLock<HashMap<String, u64>>>,
    requested_addresses: Arc<RwLock<Vec<String>>>,
}

impl MockRangeServer {
    pub fn start() -> io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let addr = listener.local_addr()?;
        let request_count = Arc::new(AtomicUsize::new(0));
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_request_count = Arc::clone(&request_count);
        let worker_shutdown = Arc::clone(&shutdown);
        let risks = Arc::new(RwLock::new(HashMap::new()));
        let requested_addresses = Arc::new(RwLock::new(Vec::new()));

        let worker_risks = Arc::clone(&risks);
        let worker_requested_addresses = Arc::clone(&requested_addresses);
        let worker = thread::spawn(move || {
            while !worker_shutdown.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut buffer = [0u8; 4096];
                        let read = stream.read(&mut buffer).unwrap_or(0);
                        let request = String::from_utf8_lossy(&buffer[..read]);

                        let body = if request.starts_with("GET /risk?")
                            && request.contains("pubkey=")
                        {
                            let pubkey = request
                                .split("pubkey=")
                                .nth(1)
                                .unwrap()
                                .split(['&', ' '])
                                .next()
                                .unwrap();
                            worker_requested_addresses
                                .write()
                                .unwrap()
                                .push(pubkey.to_string());
                            let risk_score = worker_risks
                                .read()
                                .unwrap()
                                .get(pubkey)
                                .copied()
                                .unwrap_or(0);
                            worker_request_count.fetch_add(1, Ordering::SeqCst);
                            let is_risky = risk_score > MOCK_RISK_THRESHOLD;
                            format!(
                                r#"{{"pubkey":"{pubkey}","riskScore":{risk_score},"riskThreshold":{MOCK_RISK_THRESHOLD},"isRisky":{is_risky}}}"#
                            )
                        } else {
                            r#"{"error":"not found"}"#.to_string()
                        };
                        let status = if request.starts_with("GET /risk?") {
                            "200 OK"
                        } else {
                            "404 Not Found"
                        };
                        let response = format!(
                            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        let _ = stream.write_all(response.as_bytes());
                    }
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(25));
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            base_url: format!("http://{addr}"),
            request_count,
            shutdown,
            worker: Some(worker),
            risks,
            requested_addresses,
        })
    }

    pub fn stop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }

    pub fn set_risk(&self, address: &str, risk_score: u64) {
        self.risks
            .write()
            .unwrap()
            .insert(address.to_string(), risk_score);
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn request_count(&self) -> usize {
        self.request_count.load(Ordering::SeqCst)
    }

    pub fn requested_addresses(&self) -> Vec<String> {
        self.requested_addresses.read().unwrap().clone()
    }
}

impl Drop for MockRangeServer {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn setup_validator_with_local_remote(
    ledger_path: &Path,
    programs: Option<Vec<LoadableProgram>>,
    reset_ledger: bool,
    skip_keypair_match_check: bool,
    loaded_accounts: &LoadedAccounts,
    risk_server_url: String,
) -> (TempDir, Child, IntegrationTestContext) {
    let accountsdb_config = AccountsDbConfig {
        reset: reset_ledger,
        ..Default::default()
    };

    let programs = resolve_programs(programs);

    let config = ValidatorParams {
        ledger: LedgerConfig {
            reset: reset_ledger,
            verify_keypair: !skip_keypair_match_check,
            ..Default::default()
        },
        accountsdb: accountsdb_config.clone(),
        programs,
        lifecycle: LifecycleMode::Ephemeral,
        remotes: vec![
            Remote::from_str(IntegrationTestContext::url_chain()).unwrap(),
            Remote::from_str(IntegrationTestContext::ws_url_chain()).unwrap(),
        ],
        chainlink: ChainLinkConfig {
            risk: RiskConfig {
                enabled: true,
                risk_server_url,
                ..Default::default()
            },
            ..Default::default()
        },
        storage: StorageDirectory(ledger_path.to_path_buf()),
        ..Default::default()
    };
    // Fund validator on chain
    {
        let chain_only_ctx =
            IntegrationTestContext::try_new_chain_only().unwrap();

        chain_only_ctx
            .airdrop_chain(
                &loaded_accounts.validator_authority(),
                20 * LAMPORTS_PER_SOL,
            )
            .unwrap();

        // Init fees vault for validator
        init_validator_fees_vault(
            &chain_only_ctx,
            loaded_accounts.validator_authority_keypair(),
        );
        chain_only_ctx
            .ensure_magic_fee_vault_delegated_on_chain(
                loaded_accounts.validator_authority_keypair(),
            )
            .unwrap();
    }

    let (default_tmpdir_config, Some(mut validator), port) =
        start_magicblock_validator_with_config_struct(config, loaded_accounts)
    else {
        panic!("validator should set up correctly");
    };

    let ctx = expect!(
        IntegrationTestContext::try_new_with_ephem_port(port),
        validator
    );
    (default_tmpdir_config, validator, ctx)
}

/// Init validator fees vault for proper validator setup
pub fn init_validator_fees_vault(
    chain_ctx: &IntegrationTestContext,
    validator_identity: &Keypair,
) {
    let vault_pda = dlp_api::pda::validator_fees_vault_pda_from_validator(
        &validator_identity.pubkey(),
    );
    if chain_ctx.fetch_chain_account(vault_pda).is_ok() {
        // Account exists
        return;
    }

    // DLP authority in integration tests
    let dlp_authority =
        Keypair::try_from(&DLP_TEST_AUTHORITY_BYTES[..]).unwrap();

    let latest_block_hash = chain_ctx.try_get_latest_blockhash_chain().unwrap();
    let ix = dlp_api::instruction_builder::init_validator_fees_vault(
        validator_identity.pubkey(),
        dlp_authority.pubkey(),
        validator_identity.pubkey(),
    );
    let mut tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&validator_identity.pubkey()),
        &[validator_identity, &dlp_authority],
        latest_block_hash,
    );

    chain_ctx
        .send_and_confirm_transaction_chain(
            &mut tx,
            &[validator_identity, &dlp_authority],
        )
        .unwrap();
}

pub fn token_balance_chain(
    ctx: &IntegrationTestContext,
    account: &Pubkey,
) -> u64 {
    let balance = ctx
        .try_chain_client()
        .unwrap()
        .get_token_account_balance(account)
        .unwrap();
    balance.amount.parse::<u64>().unwrap()
}

pub fn token_balance_ephem(
    ctx: &IntegrationTestContext,
    account: &Pubkey,
) -> Option<u64> {
    ctx.try_ephem_client()
        .unwrap()
        .get_token_account_balance(account)
        .ok()
        .and_then(|balance| balance.amount.parse::<u64>().ok())
}

pub fn cleanup_both(validator: &mut Child, server: &mut MockRangeServer) {
    cleanup(validator);
    server.stop();
}

pub fn delegation_record_exists(
    ctx: &IntegrationTestContext,
    delegated_account: &Pubkey,
) -> bool {
    let record_pubkey =
        dlp_api::pda::delegation_record_pda_from_delegated_account(
            delegated_account,
        );
    ctx.fetch_chain_account(record_pubkey).is_ok()
}

/// Polls until the delegation record for `delegated_account` disappears,
/// returning `true` if it did within the window.
pub fn wait_for_delegation_record_absent(
    ctx: &IntegrationTestContext,
    delegated_account: &Pubkey,
) -> bool {
    for _ in 0..60 {
        if !delegation_record_exists(ctx, delegated_account) {
            return true;
        }
        sleep(Duration::from_millis(200));
    }
    false
}

/// Polls for a window asserting the delegation record stays present the whole
/// time, returning `false` if it ever disappears.
pub fn delegation_record_persists(
    ctx: &IntegrationTestContext,
    delegated_account: &Pubkey,
) -> bool {
    for _ in 0..30 {
        if !delegation_record_exists(ctx, delegated_account) {
            return false;
        }
        sleep(Duration::from_millis(200));
    }
    true
}
