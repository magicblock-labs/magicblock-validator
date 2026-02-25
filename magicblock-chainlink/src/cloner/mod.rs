use async_trait::async_trait;
use errors::ClonerResult;
use std::ops::Deref;
use solana_account::AccountSharedData;
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;
use solana_signature::Signature;

use crate::remote_account_provider::program_account::LoadedProgram;

pub mod errors;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DelegationActions(Vec<Instruction>);

impl DelegationActions {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<Vec<Instruction>> for DelegationActions {
    fn from(value: Vec<Instruction>) -> Self {
        Self(value)
    }
}

impl From<DelegationActions> for Vec<Instruction> {
    fn from(value: DelegationActions) -> Self {
        value.0
    }
}

impl IntoIterator for DelegationActions {
    type Item = Instruction;
    type IntoIter = std::vec::IntoIter<Instruction>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl Deref for DelegationActions {
    type Target = [Instruction];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

pub struct AccountCloneRequest {
    pub pubkey: Pubkey,
    pub account: AccountSharedData,
    pub commit_frequency_ms: Option<u64>,
    pub delegation_actions: DelegationActions,
    /// If the account is delegated to another validator,
    /// this contains that validator's pubkey. None if account is not
    /// delegated to another validator.
    pub delegated_to_other: Option<Pubkey>,
}

#[async_trait]
pub trait Cloner: Send + Sync + 'static {
    /// Overrides the account in the bank to make sure it's a PDA that can be used as readonly
    /// Future transactions should be able to read from it (but not write) on the account as-is
    /// NOTE: this will run inside a separate task as to not block account sub handling.
    /// However it includes a channel callback in order to signal once the account was cloned
    /// successfully.
    async fn clone_account(
        &self,
        request: AccountCloneRequest,
    ) -> ClonerResult<Signature>;

    // Overrides the accounts in the bank to make sure the program is usable normally (and upgraded)
    // We make sure all accounts involved in the program are present in the bank with latest state
    async fn clone_program(
        &self,
        program: LoadedProgram,
    ) -> ClonerResult<Signature>;
}
