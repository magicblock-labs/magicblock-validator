use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use futures_util::future::try_join_all;
use magicblock_config::config::RiskConfig;
use reqwest::Client;
use rusqlite::{params, Connection};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct AddressRiskAssessment {
    pub is_high_risk: bool,
    pub risk_score: u64,
}

#[derive(Debug, Error)]
pub enum RiskError {
    #[error("Range risk service is enabled but api key is missing")]
    MissingApiKey,
    #[error("Failed to initialize sqlite cache at {path}: {source}")]
    SqliteInit {
        path: PathBuf,
        source: rusqlite::Error,
    },
    #[error("Failed to prepare sqlite cache directory at {path}: {source}")]
    CacheDirectory {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("Http request to range failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("Range response was not valid json: {0}")]
    InvalidJson(#[from] serde_json::Error),
    #[error("Sqlite cache read/write failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("Risk score not found in response")]
    RiskScoreNotFound,
    #[error("Addresses {0:?} are high risk")]
    HighRiskAddresses(Vec<String>),
    #[error("Task join failed: {0}")]
    Join(#[from] tokio::task::JoinError),
    #[error("Invalid risk score threshold: {0} > 10")]
    InvalidRiskScoreThreshold(u64),
}

pub type RiskResult<T> = Result<T, RiskError>;
pub type PartitionedCache =
    (Vec<(Option<u64>, String)>, Vec<(Option<u64>, String)>);

pub struct RiskService {
    client: Client,
    cache: Arc<Mutex<Connection>>,
    base_url: String,
    api_key: String,
    cache_ttl: Duration,
    risk_score_threshold: u64,
}

impl RiskService {
    pub fn try_from_config(
        config: &RiskConfig,
        ledger_path: &Path,
    ) -> RiskResult<Option<Self>> {
        if !config.enabled {
            return Ok(None);
        }
        let Some(api_key) = config.api_key.clone() else {
            return Err(RiskError::MissingApiKey);
        };
        if config.risk_score_threshold > 10 {
            return Err(RiskError::InvalidRiskScoreThreshold(
                config.risk_score_threshold,
            ));
        }

        let cache_path = ledger_path.join("risk-cache.db");
        let conn = Connection::open(&cache_path).map_err(|source| {
            RiskError::SqliteInit {
                path: cache_path.clone(),
                source,
            }
        })?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS address_risk_cache (
                address TEXT NOT NULL,
                risk_score INTEGER NOT NULL,
                fetched_at_unix_s INTEGER NOT NULL,
                PRIMARY KEY (address)
            )",
        )?;

        let client =
            Client::builder().timeout(config.request_timeout).build()?;

        Ok(Some(Self {
            client,
            cache: Arc::new(Mutex::new(conn)),
            base_url: config.base_url.trim_end_matches('/').to_string(),
            api_key,
            cache_ttl: config.cache_ttl,
            risk_score_threshold: config.risk_score_threshold,
        }))
    }

    pub async fn check_addresses(
        &self,
        addresses: Vec<String>,
    ) -> RiskResult<()> {
        let cache = self.read_cache(addresses).await?;
        let (cached_scores, uncached_addresses): PartitionedCache = cache
            .into_iter()
            .partition(|(score, _address)| score.is_some());

        let risky_cached_addresses = cached_scores
            .iter()
            .filter_map(|(score, address)| {
                if score.unwrap_or(0) >= self.risk_score_threshold {
                    Some(address.to_string())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        if !risky_cached_addresses.is_empty() {
            return Err(RiskError::HighRiskAddresses(risky_cached_addresses));
        }

        let scores =
            try_join_all(uncached_addresses.iter().map(|(_, address)| async {
                let response = self
                    .client
                    .get(format!("{}/risk/address", self.base_url))
                    .query(&[("network", "solana"), ("address", address)])
                    .bearer_auth(&self.api_key)
                    .send()
                    .await?
                    .error_for_status()?
                    .text()
                    .await?;

                let body: Value = serde_json::from_str(&response)?;
                let score = body
                    .get("riskScore")
                    .and_then(|v| v.as_u64())
                    .ok_or(RiskError::RiskScoreNotFound)?;
                Ok::<_, RiskError>((address.to_string(), score))
            }))
            .await?;

        self.write_cache(&scores).await?;

        let risky_addresses = scores
            .iter()
            .filter_map(|(address, score)| {
                if *score >= self.risk_score_threshold {
                    Some(address.to_string())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        if !risky_addresses.is_empty() {
            return Err(RiskError::HighRiskAddresses(risky_addresses));
        }

        Ok(())
    }

    async fn read_cache(
        &self,
        addresses: Vec<String>,
    ) -> RiskResult<Vec<(Option<u64>, String)>> {
        let now = now_unix_seconds();
        let max_age = self.cache_ttl.as_secs() as i64;
        let addresses = addresses.to_vec();
        let cache = Arc::clone(&self.cache);
        tokio::task::spawn_blocking(move || {
            let conn = cache.lock().expect("failed to lock cache");
            let mut results = Vec::with_capacity(addresses.len());
            for address in &addresses {
                let mut stmt = conn.prepare_cached(
                    "SELECT risk_score, fetched_at_unix_s
                 FROM address_risk_cache
                 WHERE address = ?1",
                )?;
                let result = stmt.query_row(params![address], |row| {
                    let risk_score: u64 = row.get(0)?;
                    let fetched_at: i64 = row.get(1)?;
                    Ok((risk_score, fetched_at))
                });

                match result {
                    Ok((risk_score, fetched_at)) => {
                        if now.saturating_sub(fetched_at) > max_age {
                            results.push(None);
                        } else {
                            results.push(Some(risk_score));
                        }
                    }
                    Err(rusqlite::Error::QueryReturnedNoRows) => {
                        results.push(None)
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            Ok(results.into_iter().zip(addresses).collect::<Vec<_>>())
        })
        .await?
    }

    async fn write_cache(&self, values: &[(String, u64)]) -> RiskResult<()> {
        let now = now_unix_seconds();
        let values = values.to_vec();
        let cache = Arc::clone(&self.cache);
        tokio::task::spawn_blocking(move || {
            let conn = cache.lock().expect("failed to lock cache");
            for (address, risk_score) in &values {
                conn.execute(
                    "INSERT INTO address_risk_cache
                        (address, risk_score, fetched_at_unix_s)
                     VALUES (?1, ?2, ?3)
                     ON CONFLICT(address) DO UPDATE SET
                        risk_score = excluded.risk_score,
                        fetched_at_unix_s = excluded.fetched_at_unix_s",
                    params![address, risk_score, now],
                )?;
            }
            Ok(())
        })
        .await?
    }
}

fn now_unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        io::{Read, Write},
        net::TcpListener,
        time::Duration,
    };

    use rusqlite::Connection;
    use tempfile::tempdir;
    use tokio::task::JoinHandle;

    use super::{RiskConfig, RiskError, RiskService};

    struct MockRiskServer {
        base_url: String,
        worker: JoinHandle<()>,
    }

    impl MockRiskServer {
        async fn start(
            address_scores: Vec<(String, u64)>,
            expected_calls: usize,
        ) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0")
                .expect("failed to bind mock risk server");
            let addr = listener
                .local_addr()
                .expect("failed to read mock server address");
            let score_by_address: HashMap<String, u64> =
                address_scores.into_iter().collect();

            let worker = tokio::task::spawn_blocking(move || {
                for _ in 0..expected_calls {
                    let (mut stream, _) =
                        listener.accept().expect("failed to accept request");
                    stream
                        .set_read_timeout(Some(Duration::from_secs(2)))
                        .expect("failed to set read timeout");

                    let mut buffer = [0u8; 4096];
                    let read = stream
                        .read(&mut buffer)
                        .expect("failed to read request");
                    let request = String::from_utf8_lossy(&buffer[..read]);
                    let request_lower = request.to_ascii_lowercase();
                    let address = extract_query_value(&request, "address")
                        .expect("missing address query");

                    assert!(
                        request.starts_with("GET /risk/address?"),
                        "unexpected request line: {request}"
                    );
                    assert!(
                        request.contains("network=solana"),
                        "missing network query: {request}"
                    );
                    assert!(
                        request.contains(&format!("address={address}")),
                        "missing address query: {request}"
                    );
                    assert!(
                        request_lower
                            .contains("authorization: bearer test-api-key"),
                        "missing bearer auth header: {request}"
                    );

                    let score = score_by_address.get(&address);
                    let (status, body) = match score {
                        Some(score) => {
                            ("200 OK", format!(r#"{{"riskScore":{score}}}"#))
                        }
                        None => (
                            "404 Not Found",
                            r#"{"error":"unknown address"}"#.to_string(),
                        ),
                    };
                    let response = format!(
                        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        body.len(), body
                    );
                    stream
                        .write_all(response.as_bytes())
                        .expect("failed to write response");
                }
            });

            Self {
                base_url: format!("http://{addr}"),
                worker,
            }
        }

        async fn join(self) {
            self.worker.await.expect("mock server worker panicked");
        }
    }

    fn make_risk_config(base_url: String) -> RiskConfig {
        RiskConfig {
            enabled: true,
            base_url,
            api_key: Some("test-api-key".to_string()),
            cache_ttl: Duration::from_secs(60),
            request_timeout: Duration::from_secs(2),
            risk_score_threshold: 7,
        }
    }

    fn extract_query_value(request: &str, key: &str) -> Option<String> {
        let first_line = request.lines().next()?;
        let path_and_query =
            first_line.split_whitespace().nth(1)?.split('?').nth(1)?;
        path_and_query.split('&').find_map(|part| {
            let (k, v) = part.split_once('=')?;
            if k == key {
                Some(v.to_string())
            } else {
                None
            }
        })
    }

    fn load_cached_scores(cache_path: &std::path::Path) -> Vec<(String, u64)> {
        let conn =
            Connection::open(cache_path).expect("failed to open sqlite cache");
        let mut stmt = conn
            .prepare(
                "SELECT address, risk_score
                 FROM address_risk_cache
                 ORDER BY address ASC",
            )
            .expect("failed to prepare cache read statement");
        stmt.query_map([], |row| {
            let address: String = row.get(0)?;
            let risk_score: u64 = row.get(1)?;
            Ok((address, risk_score))
        })
        .expect("failed to query cache rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("failed to collect cache rows")
    }

    #[tokio::test]
    async fn check_addresses_uses_mocked_api_and_persists_cache_in_tempdir() {
        let first = "11111111111111111111111111111111".to_string();
        let second = "33333333333333333333333333333333".to_string();
        let addresses = vec![first.clone(), second.clone()];
        let server = MockRiskServer::start(
            vec![(first.clone(), 3), (second.clone(), 4)],
            2,
        )
        .await;

        let temp_ledger = tempdir().expect("failed to create temp dir");
        let config = make_risk_config(server.base_url.clone());
        let service = RiskService::try_from_config(&config, temp_ledger.path())
            .expect("failed to create risk service")
            .expect("risk service should be enabled");

        service
            .check_addresses(addresses.clone())
            .await
            .expect("first lookup should call mocked API");
        service
            .check_addresses(addresses)
            .await
            .expect("second lookup should be served from sqlite cache");

        server.join().await;
        let cache_path = temp_ledger.path().join("risk-cache.db");
        assert!(cache_path.exists());
        let rows = load_cached_scores(&cache_path);
        assert_eq!(rows, vec![(first, 3), (second, 4)]);
    }

    #[tokio::test]
    async fn check_addresses_returns_high_risk_error_for_mocked_response() {
        let address = "22222222222222222222222222222222".to_string();
        let addresses = vec![address.clone()];
        let server = MockRiskServer::start(vec![(address.clone(), 9)], 1).await;

        let temp_ledger = tempdir().expect("failed to create temp dir");
        let config = make_risk_config(server.base_url.clone());
        let service = RiskService::try_from_config(&config, temp_ledger.path())
            .expect("failed to create risk service")
            .expect("risk service should be enabled");

        let result = service.check_addresses(addresses).await;
        assert!(matches!(
            result,
            Err(RiskError::HighRiskAddresses(ref risky)) if risky == &vec![address.clone()]
        ));

        server.join().await;
        let cache_path = temp_ledger.path().join("risk-cache.db");
        assert!(cache_path.exists());
        let rows = load_cached_scores(&cache_path);
        assert_eq!(rows, vec![(address, 9)]);
    }
}
