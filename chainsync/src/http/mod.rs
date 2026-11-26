//! Concurrent HTTP account fetches with provider failover and a shared freshness floor.

use crate::rpc::{DecodeError, Error as RpcError};

/// HTTP provider setup and fetch failures.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("at least one HTTP account provider is required")]
    MissingProviders,
    /// Failure from a configured provider, identified by its input index.
    #[error("HTTP provider {provider}: {source}")]
    Provider {
        /// Stable endpoint index supplied to the fetcher.
        provider: usize,
        /// Underlying attempt failure.
        #[source]
        source: Box<Error>,
    },
    /// Overall budget expired; `last` retains the latest attempt failure.
    #[error("fetch deadline exhausted")]
    Deadline {
        /// Latest attempt failure, if one occurred before timeout.
        #[source]
        last: Option<Box<Error>>,
    },
    #[error("HTTP status {0}")]
    Status(reqwest::StatusCode),
    #[error(transparent)]
    Request(#[from] reqwest::Error),
    #[error("{0} timed out")]
    Timeout(&'static str),
    #[error("invalid provider message: {0}")]
    Protocol(&'static str),
    #[error(transparent)]
    Json(#[from] json::Error),
    #[error(transparent)]
    Rpc(#[from] RpcError),
    #[error(transparent)]
    Account(#[from] DecodeError),
}

type Result<T> = std::result::Result<T, Error>;

pub(super) use fetcher::{FetchedAccounts, Fetcher};

impl Error {
    /// Shared classification for retry policy and provider attempt metrics.
    pub(crate) fn outcome(&self) -> Outcome {
        match self {
            Self::Timeout(_) => Outcome::Timeout,
            Self::Request(error) if error.is_timeout() => Outcome::Timeout,
            Self::Status(status) if status.as_u16() == 408 => Outcome::Timeout,
            Self::Status(status) if status.as_u16() == 429 => Outcome::RateLimited,
            Self::Rpc(RpcError { code: -32016, .. }) => Outcome::Behind,
            Self::Rpc(RpcError { code: -32005, .. }) => Outcome::Unavailable,
            Self::Status(status) if status.is_server_error() => Outcome::Unavailable,
            Self::Request(error) if !error.is_builder() && !error.is_decode() => {
                Outcome::Unavailable
            }
            Self::Protocol(_) | Self::Json(_) | Self::Account(_) => Outcome::InvalidResponse,
            Self::Request(error) if error.is_decode() => Outcome::InvalidResponse,
            _ => Outcome::Other,
        }
    }
}

/// Low-cardinality provider results shared by retry policy and metrics.
#[derive(Clone, Copy)]
pub(crate) enum Outcome {
    /// The provider returned a valid account response.
    Success,
    /// The request timed out or the provider returned HTTP 408.
    Timeout,
    /// The provider returned HTTP 429.
    RateLimited,
    /// RPC -32016 reports that the required context slot is unavailable.
    Behind,
    /// Transient transport, server, or RPC -32005 failure.
    Unavailable,
    /// Malformed protocol data, JSON, response body, or account encoding.
    InvalidResponse,
    /// A non-retryable failure outside the recognized categories.
    Other,
}

impl Outcome {
    /// Distinguishes transient endpoint failures from terminal failures.
    fn retryable(self) -> bool {
        matches!(
            self,
            Self::Timeout | Self::RateLimited | Self::Behind | Self::Unavailable
        )
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Timeout => "timeout",
            Self::RateLimited => "rate_limited",
            Self::Behind => "behind",
            Self::Unavailable => "unavailable",
            Self::InvalidResponse => "invalid_response",
            Self::Other => "other",
        }
    }
}

/// Account requests, provider failover, and shared endpoint cooldowns.
mod fetcher;

#[cfg(test)]
mod tests;

/// Maximum account entries in one getMultipleAccounts request.
pub(crate) const BATCH_LIMIT: usize = 100;
