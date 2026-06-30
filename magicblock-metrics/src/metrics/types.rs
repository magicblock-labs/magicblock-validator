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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chainlink_clone_remote_result_as_str() {
        assert_eq!(ChainlinkCloneRemoteResult::Found.as_str(), "found");
        assert_eq!(ChainlinkCloneRemoteResult::NotFound.as_str(), "not_found");
        assert_eq!(ChainlinkCloneRemoteResult::Failed.as_str(), "failed");
    }

    #[test]
    fn chainlink_clone_intent_as_str() {
        assert_eq!(
            ChainlinkCloneIntent::NormalAccount.as_str(),
            "normal_account"
        );
        assert_eq!(
            ChainlinkCloneIntent::EmptyPlaceholder.as_str(),
            "empty_placeholder"
        );
        assert_eq!(ChainlinkCloneIntent::ProgramData.as_str(), "program_data");
        assert_eq!(
            ChainlinkCloneIntent::DelegationRecord.as_str(),
            "delegation_record"
        );
        assert_eq!(ChainlinkCloneIntent::Ata.as_str(), "ata");
        assert_eq!(ChainlinkCloneIntent::Eata.as_str(), "eata");
        assert_eq!(
            ChainlinkCloneIntent::ActionDependency.as_str(),
            "action_dependency"
        );
        assert_eq!(ChainlinkCloneIntent::Unknown.as_str(), "unknown");
    }

    #[test]
    fn chainlink_clone_outcome_as_str() {
        assert_eq!(ChainlinkCloneOutcome::Submitted.as_str(), "submitted");
        assert_eq!(
            ChainlinkCloneOutcome::SubmitFailed.as_str(),
            "submit_failed"
        );
        assert_eq!(
            ChainlinkCloneOutcome::CloneSucceeded.as_str(),
            "clone_succeeded"
        );
        assert_eq!(ChainlinkCloneOutcome::CloneFailed.as_str(), "clone_failed");
        assert_eq!(ChainlinkCloneOutcome::Skipped.as_str(), "skipped");
    }

    #[test]
    fn chainlink_clone_materialization_outcome_as_str() {
        assert_eq!(
            ChainlinkCloneMaterializationOutcome::ObservedInBankAfterEnsure
                .as_str(),
            "observed_in_bank_after_ensure"
        );
        assert_eq!(
            ChainlinkCloneMaterializationOutcome::StillMissingAfterEnsure
                .as_str(),
            "still_missing_after_ensure"
        );
        assert_eq!(
            ChainlinkCloneMaterializationOutcome::RemovedAfterMaterialization
                .as_str(),
            "removed_after_materialization"
        );
    }

    #[test]
    fn chainlink_empty_placeholder_stage_as_str() {
        assert_eq!(
            ChainlinkEmptyPlaceholderStage::ConvertedToEmpty.as_str(),
            "converted_to_empty"
        );
        assert_eq!(
            ChainlinkEmptyPlaceholderStage::CloneSubmitted.as_str(),
            "clone_submitted"
        );
        assert_eq!(
            ChainlinkEmptyPlaceholderStage::CloneSubmitFailed.as_str(),
            "clone_submit_failed"
        );
        assert_eq!(
            ChainlinkEmptyPlaceholderStage::ObservedInBankAfterEnsure.as_str(),
            "observed_in_bank_after_ensure"
        );
        assert_eq!(
            ChainlinkEmptyPlaceholderStage::StillMissingAfterEnsure.as_str(),
            "still_missing_after_ensure"
        );
        assert_eq!(
            ChainlinkEmptyPlaceholderStage::LaterRefetched.as_str(),
            "later_refetched"
        );
    }
}
