use std::ops::Deref;

use borsh::{BorshDeserialize, BorshSerialize};
use light_sdk::instruction::{
    account_meta::CompressedAccountMeta, PackedAddressTreeInfo,
};

#[derive(Debug, Clone, PartialEq, BorshSerialize, BorshDeserialize)]
pub struct OptionalPackedAddressTreeInfo(Option<PackedAddressTreeInfo>);

impl Default for OptionalPackedAddressTreeInfo {
    fn default() -> Self {
        Self(None)
    }
}

impl Deref for OptionalPackedAddressTreeInfo {
    type Target = Option<PackedAddressTreeInfo>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<Option<PackedAddressTreeInfo>> for OptionalPackedAddressTreeInfo {
    fn from(value: Option<PackedAddressTreeInfo>) -> Self {
        Self(value)
    }
}

#[derive(Debug, Clone, PartialEq, BorshSerialize, BorshDeserialize)]
pub struct OptionalCompressedAccountMeta(Option<CompressedAccountMeta>);

impl Default for OptionalCompressedAccountMeta {
    fn default() -> Self {
        Self(None)
    }
}

impl Deref for OptionalCompressedAccountMeta {
    type Target = Option<CompressedAccountMeta>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<Option<CompressedAccountMeta>> for OptionalCompressedAccountMeta {
    fn from(value: Option<CompressedAccountMeta>) -> Self {
        Self(value)
    }
}
