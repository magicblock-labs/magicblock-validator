use std::{collections::HashMap, fmt};

use solana_clock::Clock;
use solana_hash::Hash;
use solana_program::instruction::InstructionError;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_transaction_error::TransactionError;

use crate::{
    intent::{BaseActionCallback, CommittedAccount},
    Slot,
};

/// Trait that provides access to system calls implemented outside of SVM,
/// accessible in magic-program.
pub trait MagicSys: Sync + Send + 'static {
    fn fetch_current_commit_nonces(
        &self,
        commits: &[CommittedAccount],
    ) -> Result<HashMap<Pubkey, u64>, InstructionError>;
}

/// Provides read access to the latest confirmed block's metadata.
/// Allows components to access block data without depending on the full ledger,
/// abstracting away the underlying storage.
pub trait LatestBlockProvider: Send + Sync + Clone + 'static {
    fn slot(&self) -> Slot;
    fn blockhash(&self) -> Hash;
    fn clock(&self) -> Clock;
}

pub trait ActionsCallbackScheduler: Send + Sync + Clone + 'static {
    type ScheduleError;

    /// Executes actions callbacks
    fn schedule(
        &self,
        callbacks: Vec<BaseActionCallback>,
        result: ActionResult,
    ) -> Vec<Result<Signature, Self::ScheduleError>>;
}

#[derive(Debug, Clone)]
pub enum ActionError {
    TimeoutError,
    ActionsError(TransactionError, Option<Signature>),
    IntentFailedError(String),
}

impl fmt::Display for ActionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TimeoutError => write!(f, "Actions expired"),
            Self::ActionsError(err, sig) => {
                write!(
                    f,
                    "User supplied actions are ill-formed: {err}. {sig:?}"
                )
            }
            Self::IntentFailedError(msg) => {
                write!(f, "Intent execution failed: {msg}")
            }
        }
    }
}

impl std::error::Error for ActionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ActionsError(err, _) => Some(err),
            _ => None,
        }
    }
}

pub type ActionResult = Result<(), ActionError>;
