//! Signer assessment does not establish delegation or authorize mutation.
//! Only high-risk verdicts reject activation; service failures propagate without entering rescue.

use std::time::Duration;

use futures::future::try_join_all;
use reqwest::{Client as HttpClient, StatusCode, redirect::Policy, retry};
use serde::{Deserialize, Serialize};
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;
use url::{Host, Url};

/// Risk-server transport configuration. Every distinct action signer is checked.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Config {
    /// Complete assessment endpoint, e.g. `https://risk.example/risk`.
    /// HTTPS is required except for loopback HTTP endpoints.
    pub endpoint: Url,
    /// Per-address request deadline, including response-body consumption.
    #[serde(with = "humantime")]
    pub timeout: Duration,
}

/// Definitive signer rejection or an unavailable/invalid assessment.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("AML endpoint must use HTTPS or loopback HTTP")]
    Endpoint,
    #[error("AML request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("AML server returned HTTP {0}")]
    Status(StatusCode),
    #[error("invalid AML response: {0}")]
    Json(#[from] json::Error),
    #[error("high-risk action signers: {0:?}")]
    Rejected(Vec<Pubkey>),
}

type Result<T> = std::result::Result<T, Error>;

/// Shared connections to the risk server; thresholds remain server-owned.
pub(crate) struct Client {
    http: HttpClient,
    endpoint: Url,
}

/// Server-owned risk verdict for one action signer.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Assessment {
    is_risky: bool,
}

impl Client {
    /// Validates HTTPS or loopback HTTP and disables redirects and implicit retries.
    pub(crate) fn new(config: Config) -> Result<Self> {
        let loopback = match config.endpoint.host() {
            Some(Host::Ipv4(ip)) => ip.is_loopback(),
            Some(Host::Ipv6(ip)) => ip.is_loopback(),
            Some(Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
            None => false,
        };
        match config.endpoint.scheme() {
            "https" => {}
            "http" if loopback => {}
            _ => return Err(Error::Endpoint),
        }
        // Do not follow redirects or hide assessment failures behind retries.
        let http = HttpClient::builder()
            .timeout(config.timeout)
            .redirect(Policy::none())
            .retry(retry::never())
            .build()?;
        Ok(Self { http, endpoint: config.endpoint })
    }

    /// Returns the signer only for a high-risk verdict; service failures remain errors.
    async fn assess(&self, pubkey: &Pubkey) -> Result<Option<Pubkey>> {
        let response = self
            .http
            .get(self.endpoint.clone())
            .query(&[("pubkey", pubkey.to_string())])
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(Error::Status(response.status()));
        }
        let assessment: Assessment = json::from_slice(&response.bytes().await?)?;
        Ok(assessment.is_risky.then_some(*pubkey))
    }
}

/// Returns high-risk signers after assessing each distinct signer concurrently.
/// Service failures are errors, not rejection verdicts. Call before acquiring the account's lease.
pub(crate) async fn check(client: &Client, actions: &[Instruction]) -> Result<Vec<Pubkey>> {
    let mut signers: Vec<_> = actions
        .iter()
        .flat_map(|action| &action.accounts)
        .filter_map(|meta| meta.is_signer.then_some(&meta.pubkey))
        .collect();
    signers.sort_unstable();
    signers.dedup();
    let checks = try_join_all(signers.into_iter().map(|key| client.assess(key)))
        .await?
        .into_iter()
        .flatten()
        .collect();
    Ok(checks)
}
