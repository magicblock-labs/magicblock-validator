use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use futures_util::future::try_join_all;
use magicblock_config::config::RiskConfig;
use reqwest::Client;
use rusqlite::{fallible_iterator::FallibleIterator, params, Connection};
use serde_json::Value;
use thiserror::Error;
use tokio::sync::Mutex;

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
}

pub type RiskResult<T> = Result<T, RiskError>;
pub type PartitionedCache<'a> = (
    Vec<(&'a Option<u64>, &'a String)>,
    Vec<(&'a Option<u64>, &'a String)>,
);

pub struct RiskService {
    client: Client,
    cache: Mutex<Connection>,
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
            cache: Mutex::new(conn),
            base_url: config.base_url.trim_end_matches('/').to_string(),
            api_key,
            cache_ttl: config.cache_ttl,
            risk_score_threshold: config.risk_score_threshold,
        }))
    }

    pub async fn check_addresses(
        &self,
        addresses: &[String],
    ) -> RiskResult<()> {
        let cache = self.read_cache(addresses).await?;
        let (cached_scores, uncached_addresses): PartitionedCache = cache
            .iter()
            .zip(addresses)
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
        addresses: &[String],
    ) -> RiskResult<Vec<Option<u64>>> {
        let now = now_unix_seconds();
        let max_age = self.cache_ttl.as_secs() as i64;
        let conn = self.cache.lock().await;
        let mut stmt = conn.prepare(
            "SELECT risk_score, fetched_at_unix_s
             FROM address_risk_cache
             WHERE address IN (?1)",
        )?;
        let rows = stmt
            .query(params![addresses.join(",")])?
            .map(|row| {
                let risk_score = row.get::<_, u64>(0)?;
                let fetched_at = row.get::<_, i64>(1)?;
                if now.saturating_sub(fetched_at) > max_age {
                    Ok(None)
                } else {
                    Ok(Some(risk_score))
                }
            })
            .collect::<Vec<_>>()?;
        Ok(rows)
    }

    async fn write_cache(&self, values: &[(String, u64)]) -> RiskResult<()> {
        let conn = self.cache.lock().await;
        let now = now_unix_seconds();
        for (address, risk_score) in values {
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
    }
}

fn now_unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
