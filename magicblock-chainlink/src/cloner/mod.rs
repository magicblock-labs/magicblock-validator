use std::ops::Deref;

use async_trait::async_trait;
use errors::ClonerResult;
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

/// Mutually exclusive post-delegation behavior for a clone request.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ClonePostDelegationMode {
    /// Clone without post-delegation actions or rescue undelegation.
    #[default]
    None,
    /// Clone and execute post-delegation actions after activation.
    ExecuteActions(DelegationActions),
    /// Clone and schedule undelegation instead of executing actions.
    ///
    /// Used for delegated accounts whose post-delegation actions cannot be
    /// executed safely, for example because they include risky signers.
    RescueUndelegate,
}

impl ClonePostDelegationMode {
    pub fn actions(&self) -> Option<&DelegationActions> {
        match self {
            Self::ExecuteActions(actions) => Some(actions),
            Self::None | Self::RescueUndelegate => None,
        }
    }

    pub fn has_actions(&self) -> bool {
        self.actions().is_some_and(|actions| !actions.is_empty())
    }

    pub fn is_rescue_undelegate(&self) -> bool {
        matches!(self, Self::RescueUndelegate)
    }
}

impl From<DelegationActions> for ClonePostDelegationMode {
    fn from(actions: DelegationActions) -> Self {
        if actions.is_empty() {
            Self::None
        } else {
            Self::ExecuteActions(actions)
        }
    }
}

#[derive(Clone)]
pub struct AccountCloneRequest {
    pub pubkey: Pubkey,
    pub account: AccountSharedData,
    pub commit_frequency_ms: Option<u64>,
    pub post_delegation_mode: ClonePostDelegationMode,
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

    /// Evicts an account from the ephemeral validator by submitting an
    /// EvictAccount transaction through the transaction pipeline.
    async fn evict_account(&self, pubkey: Pubkey) -> ClonerResult<()>;
}
