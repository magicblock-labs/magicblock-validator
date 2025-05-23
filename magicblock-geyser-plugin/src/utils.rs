use expiring_hashmap::{ExpiringHashMap as Cache, SharedMap};
use solana_sdk::{pubkey::Pubkey, signature::Signature};

use crate::types::GeyserMessage;

pub fn short_signature(sig: &Signature) -> String {
    let sig_str = sig.to_string();
    if sig_str.len() < 8 {
        "<invalid signature>".to_string()
    } else {
        format!("{}..{}", &sig_str[..8], &sig_str[sig_str.len() - 8..])
    }
}

// -----------------
// CacheState
// -----------------
#[derive(Debug, Default)]
pub(crate) enum CacheState {
    #[allow(dead_code)] // used when printing debug
    Enabled(usize),
    #[default]
    Disabled,
}

impl From<Option<&SharedMap<Signature, GeyserMessage>>> for CacheState {
    fn from(cache: Option<&SharedMap<Signature, GeyserMessage>>) -> Self {
        match cache {
            Some(cache) => CacheState::Enabled(cache.len()),
            None => CacheState::Disabled,
        }
    }
}

impl From<Option<&SharedMap<Pubkey, GeyserMessage>>> for CacheState {
    fn from(cache: Option<&SharedMap<Pubkey, GeyserMessage>>) -> Self {
        match cache {
            Some(cache) => CacheState::Enabled(cache.len()),
            None => CacheState::Disabled,
        }
    }
}

impl From<Option<&Cache<Signature, GeyserMessage>>> for CacheState {
    fn from(cache: Option<&Cache<Signature, GeyserMessage>>) -> Self {
        match cache {
            Some(cache) => CacheState::Enabled(cache.len()),
            None => CacheState::Disabled,
        }
    }
}

impl From<Option<&Cache<Pubkey, GeyserMessage>>> for CacheState {
    fn from(cache: Option<&Cache<Pubkey, GeyserMessage>>) -> Self {
        match cache {
            Some(cache) => CacheState::Enabled(cache.len()),
            None => CacheState::Disabled,
        }
    }
}
