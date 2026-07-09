//! Thin client for the risk server that assesses address risk.
//!
//! The validator no longer talks to the upstream AML provider (Range) directly.
//! Instead it queries the risk server, which owns the provider credentials, the
//! cache, and the risk threshold. This crate is a small `reqwest` wrapper that
//! asks the server whether a set of addresses is risky.

use futures_util::future::try_join_all;
use magicblock_config::config::RiskConfig;
use reqwest::Client;
use serde::Deserialize;
use thiserror::Error;

pub type RiskResult<T> = Result<T, RiskError>;

#[derive(Debug, Error)]
pub enum RiskError {
    #[error("Failed to build risk server http client: {0}")]
    ClientBuild(reqwest::Error),
    #[error("Risk server request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("Addresses {0:?} are high risk")]
    HighRiskAddresses(Vec<String>),
}

/// The risk server's assessment for a single address. Only `is_risky` is used:
/// the server owns the threshold, so the client does not re-evaluate the score.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RiskAssessment {
    is_risky: bool,
}

/// Thin client over the risk server's `GET /risk?pubkey=<addr>` endpoint.
pub struct RiskService {
    client: Client,
    base_url: String,
}

impl RiskService {
    /// Builds the client from config, returning `None` when risk checks are
    /// disabled.
    pub fn try_from_config(config: &RiskConfig) -> RiskResult<Option<Self>> {
        if !config.enabled {
            return Ok(None);
        }

        let client = Client::builder()
            .timeout(config.request_timeout)
            .build()
            .map_err(RiskError::ClientBuild)?;

        Ok(Some(Self {
            client,
            base_url: config.risk_server_url.trim_end_matches('/').to_string(),
        }))
    }

    /// Asks the risk server about each address concurrently, returning
    /// `HighRiskAddresses` if the server flags any of them as risky.
    pub async fn check_addresses(
        &self,
        addresses: Vec<String>,
    ) -> RiskResult<()> {
        let assessments = try_join_all(
            addresses.iter().map(|address| self.assess_address(address)),
        )
        .await?;

        let risky_addresses = assessments
            .into_iter()
            .zip(addresses)
            .filter_map(|(assessment, address)| {
                assessment.is_risky.then_some(address)
            })
            .collect::<Vec<_>>();
        if !risky_addresses.is_empty() {
            return Err(RiskError::HighRiskAddresses(risky_addresses));
        }

        Ok(())
    }

    async fn assess_address(
        &self,
        address: &str,
    ) -> RiskResult<RiskAssessment> {
        let assessment = self
            .client
            .get(format!("{}/risk", self.base_url))
            .query(&[("pubkey", address)])
            .send()
            .await?
            .error_for_status()?
            .json::<RiskAssessment>()
            .await?;
        Ok(assessment)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        io::{Read, Write},
        net::TcpListener,
        time::Duration,
    };

    use tokio::task::JoinHandle;

    use super::*;

    /// Minimal mock of the risk server's `GET /risk?pubkey=` endpoint. Returns
    /// a camelCase `RiskAssessment` with `isRisky` computed from the seeded
    /// score against `threshold`, mirroring the real server's contract.
    struct MockRiskServer {
        base_url: String,
        worker: JoinHandle<()>,
    }

    impl MockRiskServer {
        async fn start(
            address_scores: Vec<(String, u64)>,
            threshold: u64,
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
                    let pubkey = extract_query_value(&request, "pubkey")
                        .expect("missing pubkey query");

                    assert!(
                        request.starts_with("GET /risk?"),
                        "unexpected request line: {request}"
                    );

                    let score =
                        score_by_address.get(&pubkey).copied().unwrap_or(0);
                    let is_risky = score > threshold;
                    let body = format!(
                        r#"{{"pubkey":"{pubkey}","riskScore":{score},"riskThreshold":{threshold},"isRisky":{is_risky}}}"#
                    );
                    let response = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
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

    fn extract_query_value(request: &str, key: &str) -> Option<String> {
        let first_line = request.lines().next()?;
        let path_and_query =
            first_line.split_whitespace().nth(1)?.split('?').nth(1)?;
        path_and_query.split('&').find_map(|part| {
            let (k, v) = part.split_once('=')?;
            (k == key).then(|| v.to_string())
        })
    }

    fn make_risk_config(risk_server_url: String) -> RiskConfig {
        RiskConfig {
            enabled: true,
            risk_server_url,
            request_timeout: Duration::from_secs(2),
        }
    }

    #[tokio::test]
    async fn check_addresses_allows_low_risk_addresses() {
        magicblock_core::logger::init_for_tests();
        let first = "11111111111111111111111111111111".to_string();
        let second = "33333333333333333333333333333333".to_string();
        let addresses = vec![first.clone(), second.clone()];
        let server =
            MockRiskServer::start(vec![(first, 3), (second, 4)], 7, 2).await;

        let config = make_risk_config(server.base_url.clone());
        let service = RiskService::try_from_config(&config)
            .expect("failed to create risk service")
            .expect("risk service should be enabled");

        service
            .check_addresses(addresses)
            .await
            .expect("low-risk addresses should be allowed");

        server.join().await;
    }

    #[tokio::test]
    async fn check_addresses_returns_high_risk_error() {
        magicblock_core::logger::init_for_tests();
        let address = "22222222222222222222222222222222".to_string();
        let server =
            MockRiskServer::start(vec![(address.clone(), 9)], 7, 1).await;

        let config = make_risk_config(server.base_url.clone());
        let service = RiskService::try_from_config(&config)
            .expect("failed to create risk service")
            .expect("risk service should be enabled");

        let result = service.check_addresses(vec![address.clone()]).await;
        assert!(matches!(
            result,
            Err(RiskError::HighRiskAddresses(ref risky)) if risky == &vec![address]
        ));

        server.join().await;
    }

    #[tokio::test]
    async fn disabled_config_yields_no_service() {
        let mut config = make_risk_config("http://127.0.0.1:1".to_string());
        config.enabled = false;
        assert!(RiskService::try_from_config(&config)
            .expect("disabled config should not error")
            .is_none());
    }
}
