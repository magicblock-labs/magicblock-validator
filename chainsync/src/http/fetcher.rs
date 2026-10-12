use std::{
    sync::atomic::{AtomicU64, AtomicUsize, Ordering::*},
    time::Duration,
};

use engine::Engine;
use hyper::{body::Bytes, header::CONTENT_TYPE};
use json::LazyValue;
use reqwest::{Client, redirect::Policy, retry};
use serde::Deserialize;
use solana_account::AccountBuilder;
use solana_pubkey::Pubkey;
use tokio::time::{self, Instant};
use tracing::{error, info, warn};
use url::Url;

use crate::metrics::{self, Op};
use crate::rpc::{AccountConfig, ContextValue, Request, WireAccount};

use super::{Error, Result};

/// One confirmed RPC response with an optional account image per key, in request order.
/// Decoded images enter in `Uninit` mode; separate fetches may have different context slots.
pub(crate) struct FetchedAccounts {
    /// `None` only for an explicit RPC null; invalid accounts fail the batch.
    pub(crate) accounts: Vec<Option<AccountBuilder>>,
    /// Confirmed bank context of this response, not an account's last modification slot.
    /// Stamped on decoded images; delegation resolution may restore a recorded delegation slot.
    pub(crate) context_slot: u64,
}

/// Fetches confirmed account batches, retrying transient failures across providers on the same chain.
/// Callers split requests into batches of at most 100 keys and arrange subscriptions.
pub(crate) struct Fetcher {
    client: Client,
    /// Stable endpoint order used for error reporting.
    providers: Vec<Provider>,
    /// Supplies the confirmed chain slot used as the minimum for each HTTP fetch.
    engine: Engine,
    cursor: AtomicUsize,
    /// Monotonic origin for cooldown timestamps.
    epoch: Instant,
}

/// Endpoint whose retry cooldown is shared by all concurrent fetches.
struct Provider {
    url: Url,
    /// Milliseconds since `Fetcher::epoch` when this endpoint's cooldown ends.
    until: AtomicU64,
}

impl Fetcher {
    /// Creates shared provider cooldowns with redirects and reqwest retries disabled.
    /// Fetch attempts and their overall budget are controlled by this fetcher.
    pub(crate) fn new(providers: Vec<Url>, engine: Engine) -> Result<Self> {
        let client = Client::builder().redirect(Policy::none()).retry(retry::never()).build()?;
        let providers = providers
            .into_iter()
            .map(|url| Provider { url, until: AtomicU64::new(0) })
            .collect();
        Ok(Self {
            client,
            providers,
            engine,
            cursor: AtomicUsize::new(0),
            epoch: Instant::now(),
        })
    }

    /// Fetches 1–100 keys at confirmed commitment. Uses the greater of `min_context_slot`
    /// and Engine's confirmed chain slot as the minimum response context slot.
    /// This minimum stays fixed across retries; HTTP responses do not advance Engine's chain slot.
    ///
    /// Transient failures retry within a ten-second budget. Malformed responses
    /// fail immediately.
    pub(crate) async fn fetch(
        &self,
        keys: &[Pubkey],
        min_context_slot: Option<u64>,
    ) -> Result<FetchedAccounts> {
        let _timer = metrics::time(Op::HttpFetch);
        // Keep the same minimum slot when changing providers, so a retry cannot
        // accept a response older than this call requires.
        let min_context_slot =
            min_context_slot.unwrap_or(0).max(self.engine.accounts().chain_slot());
        let deadline = Instant::now() + OVERALL;
        // Preserve positions: getMultipleAccounts returns values in request order.
        let params = (
            keys.iter().map(ToString::to_string).collect::<Vec<_>>(),
            AccountConfig::new(Some(min_context_slot)),
        );
        let request = Request::new(1, GET_MULTIPLE_ACCOUNTS, params);
        let body = Bytes::from(json::to_vec(&request)?);
        let mut last = None;
        let mut failures = 0;
        while let Some((index, provider)) = self.available(deadline).await {
            let end = (Instant::now() + ATTEMPT).min(deadline);
            let result = self.attempt(provider, body.clone(), end, keys.len()).await;
            metrics::http_attempt(&result);
            let hostname = provider.url.host_str();
            let error = match result {
                Ok(fetched) => {
                    if failures > 0 {
                        info!(hostname, failures, min_context_slot, "HTTP fetch recovered");
                    }
                    return Ok(fetched);
                }
                Err(error) => error,
            };
            let retry = error.retryable();
            if retry {
                warn!(hostname, %error, min_context_slot, "HTTP attempt failed; retrying");
            } else {
                error!(hostname, %error, min_context_slot, "HTTP attempt failed");
            }
            let error = Error::Provider {
                provider: index,
                source: Box::new(error),
            };
            if !retry {
                return Err(error);
            }
            // Share the cooldown so other batches skip this endpoint while it recovers.
            let until = (self.epoch.elapsed() + COOLDOWN).as_millis() as u64;
            provider.until.fetch_max(until, Relaxed);
            last = Some(Box::new(error));
            failures += 1;
        }
        let error = Error::Deadline { last };
        warn!(cause = ?error, min_context_slot, failures, "HTTP retries exhausted");
        Err(error)
    }

    /// Selects a provider, waiting for the earliest cooldown only if all are cooling down; returns
    /// `None` once the overall deadline has elapsed.
    async fn available(&self, deadline: Instant) -> Option<(usize, &Provider)> {
        while Instant::now() < deadline {
            let len = self.providers.len();
            let start = self.cursor.fetch_add(1, Relaxed) % len;
            let now = self.epoch.elapsed().as_millis() as u64;
            let mut wake = deadline;
            for index in (start..len).chain(0..start) {
                let provider = &self.providers[index];
                let until = provider.until.load(Relaxed);
                if until <= now {
                    return Some((index, provider));
                }
                wake = wake.min(self.epoch + Duration::from_millis(until));
            }
            time::sleep_until(wake).await;
        }
        None
    }

    /// Makes one provider request and decodes its account images.
    /// Rejects a mismatched account count before decoding images so request positions stay valid.
    async fn attempt(
        &self,
        provider: &Provider,
        body: Bytes,
        end: Instant,
        expected: usize,
    ) -> Result<FetchedAccounts> {
        let _timer = metrics::time(Op::HttpAttempt);
        let response = self
            .client
            .post(provider.url.clone())
            .header(CONTENT_TYPE, "application/json")
            .body(body)
            .timeout(end.saturating_duration_since(Instant::now()))
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(Error::Status(response.status()));
        }
        let bytes = response.bytes().await?;
        let response: Response<'_> = json::from_slice(&bytes)?;
        if let Some(error) = response.error {
            return Err(Error::Rpc(json::from_str(error.as_raw_str())?));
        }
        let result = response.result.ok_or(Error::Protocol("missing HTTP result"))?;
        let result: ContextValue<Vec<Option<WireAccount<'_>>>> =
            json::from_str(result.as_raw_str())?;
        if result.value.len() != expected {
            return Err(Error::Protocol("account result count differs from request"));
        }
        let context_slot = result.context.slot;
        let mut accounts = Vec::with_capacity(expected);
        for account in result.value {
            accounts.push(account.map(|account| account.decode(context_slot)).transpose()?);
        }
        Ok(FetchedAccounts { accounts, context_slot })
    }
}

/// Borrowed success or provider rejection before payload decoding.
#[derive(Deserialize)]
struct Response<'a> {
    /// Success payload decoded after outer-envelope validation.
    #[serde(borrow)]
    result: Option<LazyValue<'a>>,
    /// Structured provider rejection retained for diagnostics.
    #[serde(borrow)]
    error: Option<LazyValue<'a>>,
}

/// RPC operation that returns one shared context for the batch.
const GET_MULTIPLE_ACCOUNTS: &str = "getMultipleAccounts";
/// Total budget across attempts and provider cooldowns.
const OVERALL: Duration = Duration::from_secs(10);
/// Per-provider attempt budget, capped by the overall deadline.
const ATTEMPT: Duration = Duration::from_secs(2);
/// Shared delay after transient provider failure.
const COOLDOWN: Duration = Duration::from_millis(100);
