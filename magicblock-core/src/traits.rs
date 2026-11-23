use std::{collections::HashMap, fmt};

use solana_clock::Clock;
use solana_hash::Hash;
use solana_program::instruction::InstructionError;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_transaction_error::TransactionError;
use tokio::sync::broadcast;

use crate::{
    intent::{types::CommittedAccount, BaseActionCallback, MagicIntentBundle},
    link::blocks::LatestBlockInner,
    Slot,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntentSizeStage {
    Commit,
    Finalize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FormatFitError {
    AccountKeyLimitExceeded { actual: usize, limit: usize },
    InstructionCountLimitExceeded { actual: usize, limit: usize },
    AccountLockLimitExceeded { actual: usize, limit: usize },
    WireSizeExceeded { actual: usize, limit: usize },
    MessageCannotBeCompiled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IntentSizeError {
    ComputeBudgetExceeded {
        stage: IntentSizeStage,
        estimated: u32,
        limit: u32,
    },
    NoSupportedFormatFits {
        stage: IntentSizeStage,
        v1: FormatFitError,
        v0_with_alts: FormatFitError,
    },
    FitCheckFailed {
        stage: IntentSizeStage,
        reason: String,
    },
}

impl IntentSizeError {
    pub fn is_fit_limit(&self) -> bool {
        !matches!(self, Self::FitCheckFailed { .. })
    }
}

impl fmt::Display for IntentSizeStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Commit => f.write_str("commit"),
            Self::Finalize => f.write_str("finalize"),
        }
    }
}

impl fmt::Display for FormatFitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AccountKeyLimitExceeded { actual, limit } => {
                write!(f, "account keys {actual} exceed limit {limit}")
            }
            Self::InstructionCountLimitExceeded { actual, limit } => {
                write!(f, "instruction count {actual} exceeds limit {limit}")
            }
            Self::AccountLockLimitExceeded { actual, limit } => {
                write!(f, "account locks {actual} exceed limit {limit}")
            }
            Self::WireSizeExceeded { actual, limit } => {
                write!(f, "wire size {actual} bytes exceeds limit {limit}")
            }
            Self::MessageCannotBeCompiled => {
                f.write_str("message cannot be compiled or validated")
            }
        }
    }
}

impl fmt::Display for IntentSizeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ComputeBudgetExceeded {
                stage,
                estimated,
                limit,
            } => write!(
                f,
                "{stage} stage compute estimate {estimated} CU exceeds limit {limit} CU"
            ),
            Self::NoSupportedFormatFits {
                stage,
                v1,
                v0_with_alts,
            } => write!(
                f,
                "{stage} stage fits neither transaction format (v1: {v1}; v0+ALT: {v0_with_alts})"
            ),
            Self::FitCheckFailed { stage, reason } => {
                write!(f, "{stage} stage fit check failed: {reason}")
            }
        }
    }
}

impl std::error::Error for IntentSizeError {}

/// Trait that provides access to system calls implemented outside of SVM,
/// accessible in magic-program.
pub trait MagicSys: Sync + Send + 'static {
    fn fetch_current_commit_nonces(
        &self,
        commits: &[CommittedAccount],
    ) -> Result<HashMap<Pubkey, u64>, InstructionError>;

    /// Returns `Ok(())` if `intent` could ever fit on the base layer, even
    /// under the smallest possible task representation (buffer-mode commits,
    /// address lookup tables, 2-stage execution). If it can never fit no
    /// matter how it gets optimized at execution time, returns the failed
    /// stage and format checks so the intent can be refused up front instead
    /// of failing later during commit/finalize.
    fn validate_intent_size(
        &self,
        intent: &MagicIntentBundle,
    ) -> Result<(), IntentSizeError>;
}

/// Provides read access to the latest confirmed block's metadata.
/// Allows components to access block data without depending on the full ledger,
/// abstracting away the underlying storage.
pub trait LatestBlockProvider: Send + Sync + Clone + 'static {
    fn slot(&self) -> Slot;
    fn blockhash(&self) -> Hash;
    fn clock(&self) -> Clock;
    fn subscribe(&self) -> broadcast::Receiver<LatestBlockInner>;
}

/// Interface for service handling callback execution/scheduling
pub trait ActionsCallbackScheduler: Send + Sync + Clone + 'static {
    /// Schedules callback txs.
    /// Returns signature or construction error per callback.
    fn schedule(
        &self,
        callbacks: Vec<BaseActionCallback>,
        signature: Option<Signature>,
        result: ActionResult,
    ) -> Vec<Result<Signature, CallbackScheduleError>>;
}

#[derive(Debug)]
pub enum CallbackScheduleError {
    SerializationError(bincode::Error),
    SigningError(String),
}

impl fmt::Display for CallbackScheduleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SerializationError(err) => {
                write!(f, "Callback serialization failed: {err}")
            }
            Self::SigningError(msg) => {
                write!(f, "Callback signing failed: {msg}")
            }
        }
    }
}

impl std::error::Error for CallbackScheduleError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::SerializationError(err) => Some(err),
            Self::SigningError(_) => None,
        }
    }
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
