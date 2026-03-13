//! AccountsDb snapshot with positioning metadata.

use std::collections::HashMap;

use async_nats::jetstream::object_store;
use magicblock_core::Slot;

use super::cfg;
use crate::{Error, Result};

/// AccountsDb snapshot with positioning metadata.
#[derive(Debug)]
pub struct Snapshot {
    /// Raw snapshot bytes.
    pub data: Vec<u8>,
    /// Slot at which the snapshot was taken.
    pub slot: Slot,
}

/// Metadata stored with each snapshot object.
pub(crate) struct SnapshotMeta {
    pub(crate) slot: Slot,
    pub(crate) sequence: u64,
}

impl SnapshotMeta {
    /// Parses required metadata fields from object info.
    pub(crate) fn parse(info: &object_store::ObjectInfo) -> Result<Self> {
        let get_parsed =
            |key: &str| info.metadata.get(key).and_then(|v| v.parse().ok());

        let slot = get_parsed(cfg::META_SLOT).ok_or_else(|| {
            Error::Internal("missing 'slot' in snapshot metadata".into())
        })?;
        let sequence = get_parsed(cfg::META_SEQUENCE).ok_or_else(|| {
            Error::Internal("missing 'sequence' in snapshot metadata".into())
        })?;

        Ok(Self { slot, sequence })
    }

    pub(crate) fn into_headers(self) -> HashMap<String, String> {
        HashMap::from([
            (cfg::META_SLOT.into(), self.slot.to_string()),
            (cfg::META_SEQUENCE.into(), self.sequence.to_string()),
        ])
    }
}
