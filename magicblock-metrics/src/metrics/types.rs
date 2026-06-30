use std::fmt;

use solana_signature::Signature;

// -----------------
// Outcome
// -----------------
const OUTCOME_SUCCESS: &str = "success";
const OUTCOME_ERROR: &str = "error";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Success,
    Error,
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use Outcome::*;
        match self {
            Success => write!(f, "{OUTCOME_SUCCESS}"),
            Error => write!(f, "{OUTCOME_ERROR}"),
        }
    }
}

impl Outcome {
    pub fn as_str(&self) -> &str {
        use Outcome::*;
        match self {
            Success => OUTCOME_SUCCESS,
            Error => OUTCOME_ERROR,
        }
    }

    pub fn from_success(success: bool) -> Self {
        if success {
            Outcome::Success
        } else {
            Outcome::Error
        }
    }
}

impl LabelValue for Outcome {
    fn value(&self) -> &str {
        self.as_str()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountFetchOrigin {
    GetMultipleAccounts,
    GetAccount,
    SendTransaction(Signature),
    ProjectAta,
}

impl AccountFetchOrigin {
    pub fn as_str(&self) -> &str {
        use AccountFetchOrigin::*;
        match self {
            GetMultipleAccounts => "get_multiple_accounts",
            GetAccount => "get_account",
            SendTransaction(_) => "send_transaction",
            ProjectAta => "project_ata",
        }
    }

    pub fn signature(&self) -> Option<&Signature> {
        match self {
            Self::SendTransaction(sig) => Some(sig),
            _ => None,
        }
    }
}

impl fmt::Display for AccountFetchOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl LabelValue for AccountFetchOrigin {
    fn value(&self) -> &str {
        self.as_str()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainlinkCloneRemoteResult {
    Found,
    NotFound,
    Failed,
}

impl ChainlinkCloneRemoteResult {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Found => "found",
            Self::NotFound => "not_found",
            Self::Failed => "failed",
        }
    }
}

impl fmt::Display for ChainlinkCloneRemoteResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl LabelValue for ChainlinkCloneRemoteResult {
    fn value(&self) -> &str {
        self.as_str()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainlinkCloneIntent {
    NormalAccount,
    EmptyPlaceholder,
    ProgramData,
    DelegationRecord,
    Ata,
    Eata,
    ActionDependency,
    Unknown,
}

impl ChainlinkCloneIntent {
    pub fn as_str(&self) -> &str {
        match self {
            Self::NormalAccount => "normal_account",
            Self::EmptyPlaceholder => "empty_placeholder",
            Self::ProgramData => "program_data",
            Self::DelegationRecord => "delegation_record",
            Self::Ata => "ata",
            Self::Eata => "eata",
            Self::ActionDependency => "action_dependency",
            Self::Unknown => "unknown",
        }
    }
}

impl fmt::Display for ChainlinkCloneIntent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl LabelValue for ChainlinkCloneIntent {
    fn value(&self) -> &str {
        self.as_str()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainlinkCloneOutcome {
    Submitted,
    SubmitFailed,
    CloneSucceeded,
    CloneFailed,
    Skipped,
}

impl ChainlinkCloneOutcome {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Submitted => "submitted",
            Self::SubmitFailed => "submit_failed",
            Self::CloneSucceeded => "clone_succeeded",
            Self::CloneFailed => "clone_failed",
            Self::Skipped => "skipped",
        }
    }
}

impl fmt::Display for ChainlinkCloneOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl LabelValue for ChainlinkCloneOutcome {
    fn value(&self) -> &str {
        self.as_str()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainlinkCloneMaterializationOutcome {
    ObservedInBankAfterEnsure,
    StillMissingAfterEnsure,
    RemovedAfterMaterialization,
}

impl ChainlinkCloneMaterializationOutcome {
    pub fn as_str(&self) -> &str {
        match self {
            Self::ObservedInBankAfterEnsure => "observed_in_bank_after_ensure",
            Self::StillMissingAfterEnsure => "still_missing_after_ensure",
            Self::RemovedAfterMaterialization => {
                "removed_after_materialization"
            }
        }
    }
}

impl fmt::Display for ChainlinkCloneMaterializationOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl LabelValue for ChainlinkCloneMaterializationOutcome {
    fn value(&self) -> &str {
        self.as_str()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainlinkEmptyPlaceholderStage {
    ConvertedToEmpty,
    CloneSubmitted,
    CloneSubmitFailed,
    ObservedInBankAfterEnsure,
    StillMissingAfterEnsure,
    /// Reserved for a future sampled/sketch implementation; current code does not retain per-pubkey state.
    LaterRefetched,
}

impl ChainlinkEmptyPlaceholderStage {
    pub fn as_str(&self) -> &str {
        match self {
            Self::ConvertedToEmpty => "converted_to_empty",
            Self::CloneSubmitted => "clone_submitted",
            Self::CloneSubmitFailed => "clone_submit_failed",
            Self::ObservedInBankAfterEnsure => "observed_in_bank_after_ensure",
            Self::StillMissingAfterEnsure => "still_missing_after_ensure",
            Self::LaterRefetched => "later_refetched",
        }
    }
}

impl fmt::Display for ChainlinkEmptyPlaceholderStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl LabelValue for ChainlinkEmptyPlaceholderStage {
    fn value(&self) -> &str {
        self.as_str()
    }
}

// -----------------
// AccountCommit
// -----------------
pub enum AccountCommit<'a> {
    CommitOnly { pubkey: &'a str, outcome: Outcome },
    CommitAndUndelegate { pubkey: &'a str, outcome: Outcome },
}

pub trait LabelValue {
    fn value(&self) -> &str;
}

impl LabelValue for &str {
    fn value(&self) -> &str {
        self
    }
}

impl LabelValue for String {
    fn value(&self) -> &str {
        self
    }
}

impl<T, E> LabelValue for Result<T, E>
where
    T: LabelValue,
    E: LabelValue,
{
    fn value(&self) -> &str {
        match self {
            Ok(ok) => ok.value(),
            Err(err) => err.value(),
        }
    }
}
