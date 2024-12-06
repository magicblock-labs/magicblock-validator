use expiring_hashmap::{ExpiringHashMap as Cache, SharedMap};
use geyser_grpc_proto::geyser::SubscribeUpdateTransaction;
use solana_sdk::{pubkey::Pubkey, signature::Signature};

use crate::types::GeyserMessage;

pub fn short_signature_from_sub_update(
    tx: &SubscribeUpdateTransaction,
) -> String {
    tx.transaction
        .as_ref()
        .map(|x| short_signature_from_vec(&x.signature))
        .unwrap_or("<missing transaction>".to_string())
}

pub fn short_signature_from_vec(sig: &[u8]) -> String {
    match Signature::try_from(sig) {
        Ok(sig) => short_signature(&sig),
        Err(_) => "<invalid signature>".to_string(),
    }
}

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
