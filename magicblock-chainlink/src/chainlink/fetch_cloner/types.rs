use std::{collections::HashSet, fmt};

use solana_account::AccountSharedData;
use solana_pubkey::Pubkey;

use crate::{
    cloner::AccountCloneRequest,
    remote_account_provider::{
        program_account::LoadedProgram, ResolvedAccountSharedData,
    },
};

pub(crate) struct AccountWithCompanion {
    pub(crate) pubkey: Pubkey,
    pub(crate) account: ResolvedAccountSharedData,
    pub(crate) companion_pubkey: Pubkey,
    pub(crate) companion_account: Option<ResolvedAccountSharedData>,
}

pub(crate) enum RefreshDecision {
    No,
    Yes,
    YesAndMarkEmptyIfNotFound,
}

// Pipeline helper types
pub(crate) struct ExistingSubs {
    pub(crate) existing_subs: HashSet<Pubkey>,
}

pub(crate) struct ClassifiedAccounts {
    pub(crate) not_found: Vec<(Pubkey, u64)>,
    pub(crate) plain: Vec<AccountCloneRequest>,
    pub(crate) owned_by_deleg: Vec<(Pubkey, AccountSharedData, u64)>,
    pub(crate) owned_by_deleg_compressed: Vec<(Pubkey, AccountSharedData, u64)>,
    pub(crate) programs: Vec<(Pubkey, AccountSharedData, u64)>,
    pub(crate) atas: Vec<(
        Pubkey,
        AccountSharedData,
        magicblock_core::token_programs::AtaInfo,
        u64,
    )>,
}

pub(crate) struct ResolvedDelegatedAccounts {
    pub(crate) accounts_to_clone: Vec<AccountCloneRequest>,
    pub(crate) record_subs: Vec<Pubkey>,
    pub(crate) missing_delegation_record: Vec<(Pubkey, u64)>,
}

pub(crate) struct ResolvedPrograms {
    pub(crate) loaded_programs: Vec<LoadedProgram>,
    pub(crate) program_data_subs: HashSet<Pubkey>,
}

pub(crate) struct PartitionedNotFound {
    pub(crate) clone_as_empty: Vec<(Pubkey, u64)>,
    pub(crate) not_found: Vec<(Pubkey, u64)>,
}

#[derive(Debug, Default)]
pub struct FetchAndCloneResult {
    pub not_found_on_chain: Vec<(Pubkey, u64)>,
    pub missing_delegation_record: Vec<(Pubkey, u64)>,
}

impl FetchAndCloneResult {
    pub fn pubkeys_not_found_on_chain(&self) -> Vec<Pubkey> {
        self.not_found_on_chain.iter().map(|(p, _)| *p).collect()
    }

    pub fn pubkeys_missing_delegation_record(&self) -> Vec<Pubkey> {
        self.missing_delegation_record
            .iter()
            .map(|(p, _)| *p)
            .collect()
    }

    pub fn is_ok(&self) -> bool {
        self.not_found_on_chain.is_empty()
            && self.missing_delegation_record.is_empty()
    }
}

impl fmt::Display for FetchAndCloneResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_ok() {
            write!(f, "All accounts fetched and cloned successfully")
        } else {
            if !self.not_found_on_chain.is_empty() {
                writeln!(
                    f,
                    "Accounts not found on chain: {:?}",
                    self.not_found_on_chain
                        .iter()
                        .map(|(p, _)| p.to_string())
                        .collect::<Vec<_>>()
                )?;
            }
            if !self.missing_delegation_record.is_empty() {
                writeln!(
                    f,
                    "Accounts missing delegation record: {:?}",
                    self.missing_delegation_record
                        .iter()
                        .map(|(p, _)| p.to_string())
                        .collect::<Vec<_>>()
                )?;
            }
            Ok(())
        }
    }
}
