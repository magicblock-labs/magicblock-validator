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

pub(crate) struct JsonBody(pub Vec<u8>);

impl<S: Serialize> From<S> for JsonBody {
    fn from(value: S) -> Self {
        // NOTE: json to vec serialization is infallible, so the
        // unwrap is there to avoid an eyesore of panicking code
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

#[derive(PartialEq, PartialOrd, Ord, Eq, Clone)]
pub(crate) enum ProgramFilter {
    DataSize(usize),
    MemCmp { offset: usize, bytes: Vec<u8> },
}

#[derive(PartialEq, PartialOrd, Ord, Eq, Clone, Default)]
pub(crate) struct ProgramFilters(Vec<ProgramFilter>);

impl ProgramFilter {
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
    pub(crate) fn push(&mut self, filter: ProgramFilter) {
        self.0.push(filter)
    }

    #[inline]
    pub(crate) fn matches(&self, data: &[u8]) -> bool {
        self.0.iter().all(|f| f.matches(data))
    }
}

impl From<Option<Vec<RpcFilterType>>> for ProgramFilters {
    fn from(value: Option<Vec<RpcFilterType>>) -> Self {
        let Some(filters) = value else {
            return Self(vec![]);
        };
        let mut inner = Vec::with_capacity(filters.len());
        for f in filters {
            match f {
                RpcFilterType::DataSize(len) => {
                    inner.push(ProgramFilter::DataSize(len as usize));
                }
                RpcFilterType::Memcmp(memcmp) => {
                    inner.push(ProgramFilter::MemCmp {
                        offset: memcmp.offset(),
                        bytes: memcmp.bytes().unwrap_or_default().to_vec(),
                    });
                }
                _ => continue,
            }
        }
        Self(inner)
    }
}
#[derive(Serialize)]
pub(crate) struct AccountWithPubkey {
    pubkey: Serde32Bytes,
    account: UiAccount,
}

impl AccountWithPubkey {
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
