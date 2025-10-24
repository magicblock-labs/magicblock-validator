use std::{
    convert::Infallible,
    pin::Pin,
    task::{Context, Poll},
};

use hyper::body::{Body, Bytes, Frame, SizeHint};
use json::Serialize;
use magicblock_core::link::accounts::LockedAccount;
use solana_account_decoder::{
    encode_ui_account, UiAccount, UiAccountEncoding, UiDataSliceConfig,
};
use solana_rpc_client_api::filter::RpcFilterType;

use crate::requests::params::Serde32Bytes;

/// A newtype wrapper for a `Vec<u8>` that implements Hyper's `Body` trait.
/// This is used to efficiently send already-serialized JSON as an HTTP response body.
pub(crate) struct JsonBody(pub Vec<u8>);

impl<S: Serialize> From<S> for JsonBody {
    fn from(value: S) -> Self {
        // Serialization to a Vec<u8> is infallible for the types used.
        let serialized = json::to_vec(&value).unwrap_or_default();
        Self(serialized)
    }
}

impl Body for JsonBody {
    type Data = Bytes;
    type Error = Infallible;

    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(self.0.len() as u64)
    }

    /// Sends the entire body as a single data frame.
    fn poll_frame(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        if !self.0.is_empty() {
            let s = std::mem::take(&mut self.0);
            Poll::Ready(Some(Ok(Frame::data(s.into()))))
        } else {
            Poll::Ready(None)
        }
    }
}

/// A single, server-side filter for `getProgramAccounts`.
#[derive(PartialEq, PartialOrd, Ord, Eq, Clone)]
pub(crate) enum ProgramFilter {
    DataSize(usize),
    MemCmp { offset: usize, bytes: Vec<u8> },
}

/// A collection of server-side filters for `getProgramAccounts`.
#[derive(PartialEq, PartialOrd, Ord, Eq, Clone, Default)]
pub(crate) struct ProgramFilters(Vec<ProgramFilter>);

impl ProgramFilter {
    /// Checks if an account's data matches this filter's criteria.
    pub(crate) fn matches(&self, data: &[u8]) -> bool {
        match self {
            Self::DataSize(len) => data.len() == *len,
            Self::MemCmp { offset, bytes } => {
                if let Some(slice) = data.get(*offset..*offset + bytes.len()) {
                    slice == bytes
                } else {
                    false
                }
            }
        }
    }
}

impl ProgramFilters {
    /// Add new filter to the list
    pub(crate) fn push(&mut self, filter: ProgramFilter) {
        self.0.push(filter)
    }
    /// Checks if a given data slice satisfies all configured filters.
    #[inline]
    pub(crate) fn matches(&self, data: &[u8]) -> bool {
        self.0.iter().all(|f| f.matches(data))
    }
}

/// Converts the client-facing `RpcFilterType` configuration into the
/// internal `ProgramFilters` representation.
impl From<Option<Vec<RpcFilterType>>> for ProgramFilters {
    fn from(value: Option<Vec<RpcFilterType>>) -> Self {
        let Some(filters) = value else {
            return Self::default();
        };

        // Convert the RPC filters into our internal, optimized format.
        let inner = filters
            .into_iter()
            .filter_map(|f| match f {
                RpcFilterType::DataSize(len) => {
                    Some(ProgramFilter::DataSize(len as usize))
                }
                RpcFilterType::Memcmp(memcmp) => {
                    let bytes = memcmp.bytes().unwrap_or_default().into_owned();
                    Some(ProgramFilter::MemCmp {
                        offset: memcmp.offset(),
                        bytes,
                    })
                }
                // for now we don't support other filter types
                _ => None,
            })
            .collect();
        Self(inner)
    }
}

/// A struct that pairs a pubkey with its encoded `UiAccount`, used for RPC responses.
#[derive(Serialize)]
pub(crate) struct AccountWithPubkey {
    pubkey: Serde32Bytes,
    account: UiAccount,
}

impl AccountWithPubkey {
    /// Constructs a new `AccountWithPubkey`, performing a
    /// race-free read and encoding of the account data.
    pub(crate) fn new(
        account: &LockedAccount,
        encoding: UiAccountEncoding,
        slice: Option<UiDataSliceConfig>,
    ) -> Self {
        let pubkey = account.pubkey.into();
        let account = account.read_locked(|pk, acc| {
            encode_ui_account(pk, acc, encoding, None, slice)
        });
        Self { pubkey, account }
    }
}
