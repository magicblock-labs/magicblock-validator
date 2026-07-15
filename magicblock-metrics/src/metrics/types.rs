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

// -----------------
// AccountClone
// -----------------
pub enum AccountClone<'a> {
    FeePayer {
        pubkey: &'a str,
        balance_pda: Option<&'a str>,
    },
    Undelegated {
        pubkey: &'a str,
        owner: &'a str,
    },
    Delegated {
        pubkey: &'a str,
        owner: &'a str,
    },
    Program {
        pubkey: &'a str,
    },
}

// -----------------
// AccountCommit
// -----------------
pub enum AccountCommit<'a> {
    CommitOnly { pubkey: &'a str, outcome: Outcome },
    CommitAndUndelegate { pubkey: &'a str, outcome: Outcome },
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
pub enum ChainlinkPendingFetchLayer {
    FetchCloner,
    RemoteAccountProvider,
}

impl ChainlinkPendingFetchLayer {
    pub fn as_str(&self) -> &str {
        match self {
            Self::FetchCloner => "fetch_cloner",
            Self::RemoteAccountProvider => "remote_account_provider",
        }
    }
}

impl fmt::Display for ChainlinkPendingFetchLayer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl LabelValue for ChainlinkPendingFetchLayer {
    fn value(&self) -> &str {
        self.as_str()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainlinkPendingFetchOutcome {
    Owned,
    JoinedExisting,
    OwnerSucceeded,
    OwnerFailed,
    OwnerCancelled,
    ResolvedBySubscriptionUpdate,
    RpcFetchCompletedAfterUpdate,
}

impl ChainlinkPendingFetchOutcome {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Owned => "owned",
            Self::JoinedExisting => "joined_existing",
            Self::OwnerSucceeded => "owner_succeeded",
            Self::OwnerFailed => "owner_failed",
            Self::OwnerCancelled => "owner_cancelled",
            Self::ResolvedBySubscriptionUpdate => {
                "resolved_by_subscription_update"
            }
            Self::RpcFetchCompletedAfterUpdate => {
                "rpc_fetch_completed_after_update"
            }
        }
    }
}

impl fmt::Display for ChainlinkPendingFetchOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl LabelValue for ChainlinkPendingFetchOutcome {
    fn value(&self) -> &str {
        self.as_str()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BankPrecheckOutcome {
    BankHitNoFetch,
    BankHitUndelegatingRefreshRequired,
    BankMissRemoteRequired,
    ForcedRefreshRemoteRequired,
}

impl BankPrecheckOutcome {
    pub fn as_str(&self) -> &str {
        match self {
            Self::BankHitNoFetch => "bank_hit_no_fetch",
            Self::BankHitUndelegatingRefreshRequired => {
                "bank_hit_undelegating_refresh_required"
            }
            Self::BankMissRemoteRequired => "bank_miss_remote_required",
            Self::ForcedRefreshRemoteRequired => {
                "forced_refresh_remote_required"
            }
        }
    }
}

impl fmt::Display for BankPrecheckOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl LabelValue for BankPrecheckOutcome {
    fn value(&self) -> &str {
        self.as_str()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BankPrecheckReason {
    Absent,
    NonUndelegatingPresent,
    UndelegatingStillValid,
    UndelegatingCheckTimeout,
    UndelegatingRefresh,
    ForcedRefresh,
}

impl BankPrecheckReason {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Absent => "absent",
            Self::NonUndelegatingPresent => "non_undelegating_present",
            Self::UndelegatingStillValid => "undelegating_still_valid",
            Self::UndelegatingCheckTimeout => "undelegating_check_timeout",
            Self::UndelegatingRefresh => "undelegating_refresh",
            Self::ForcedRefresh => "forced_refresh",
        }
    }
}

impl fmt::Display for BankPrecheckReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl LabelValue for BankPrecheckReason {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubscriptionRegistrationOrigin {
    Fetch(AccountFetchOrigin),
    Internal,
}

impl SubscriptionRegistrationOrigin {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Fetch(origin) => origin.as_str(),
            Self::Internal => "internal",
        }
    }
}

impl fmt::Display for SubscriptionRegistrationOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl LabelValue for SubscriptionRegistrationOrigin {
    fn value(&self) -> &str {
        self.as_str()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubscriptionReasonLabel {
    DirectAccount,
    DelegationRecord,
    ProgramData,
    UndelegationTracking,
    AtaProjection,
}

impl SubscriptionReasonLabel {
    pub fn as_str(&self) -> &str {
        match self {
            Self::DirectAccount => "direct_account",
            Self::DelegationRecord => "delegation_record",
            Self::ProgramData => "program_data",
            Self::UndelegationTracking => "undelegation_tracking",
            Self::AtaProjection => "ata_projection",
        }
    }
}

impl fmt::Display for SubscriptionReasonLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl LabelValue for SubscriptionReasonLabel {
    fn value(&self) -> &str {
        self.as_str()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubscriptionRegistrationOutcome {
    AlreadyPresent,
    AddedBelowCapacity,
    EvictedCandidate,
    SubscribeError,
    UnsubscribeEvictedError,
    RejectedNoCapacity,
    RejectedAndUnsubscribed,
    UnsubscribeRejectedError,
}

impl SubscriptionRegistrationOutcome {
    pub fn as_str(&self) -> &str {
        match self {
            Self::AlreadyPresent => "already_present",
            Self::AddedBelowCapacity => "added_below_capacity",
            Self::EvictedCandidate => "evicted_candidate",
            Self::SubscribeError => "subscribe_error",
            Self::UnsubscribeEvictedError => "unsubscribe_evicted_error",
            Self::RejectedNoCapacity => "rejected_no_capacity",
            Self::RejectedAndUnsubscribed => "rejected_and_unsubscribed",
            Self::UnsubscribeRejectedError => "unsubscribe_rejected_error",
        }
    }
}

impl fmt::Display for SubscriptionRegistrationOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl LabelValue for SubscriptionRegistrationOutcome {
    fn value(&self) -> &str {
        self.as_str()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubscriptionReleaseOutcome {
    Unsubscribed,
    AlreadyAbsent,
    UnsubscribeFailed,
    RetainedIntentionally,
    RetainedOtherReasons,
}

impl SubscriptionReleaseOutcome {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Unsubscribed => "unsubscribed",
            Self::AlreadyAbsent => "already_absent",
            Self::UnsubscribeFailed => "unsubscribe_failed",
            Self::RetainedIntentionally => "retained_intentionally",
            Self::RetainedOtherReasons => "retained_other_reasons",
        }
    }
}

impl fmt::Display for SubscriptionReleaseOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl LabelValue for SubscriptionReleaseOutcome {
    fn value(&self) -> &str {
        self.as_str()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubscriptionCleanupSource {
    NormalRelease,
    ManualUnsubscribe,
    CapacityEviction,
    RejectedNewSubscription,
    DelegatedAccountSilent,
    Reconciler,
}

impl SubscriptionCleanupSource {
    pub fn as_str(&self) -> &str {
        match self {
            Self::NormalRelease => "normal_release",
            Self::ManualUnsubscribe => "manual_unsubscribe",
            Self::CapacityEviction => "capacity_eviction",
            Self::RejectedNewSubscription => "rejected_new_subscription",
            Self::DelegatedAccountSilent => "delegated_account_silent",
            Self::Reconciler => "reconciler",
        }
    }
}

impl fmt::Display for SubscriptionCleanupSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl LabelValue for SubscriptionCleanupSource {
    fn value(&self) -> &str {
        self.as_str()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubscriptionCleanupOutcome {
    Unsubscribed,
    AlreadyAbsent,
    UnsubscribeFailed,
    RemovalUpdateFailed,
    RetainedIntentionally,
}

impl SubscriptionCleanupOutcome {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Unsubscribed => "unsubscribed",
            Self::AlreadyAbsent => "already_absent",
            Self::UnsubscribeFailed => "unsubscribe_failed",
            Self::RemovalUpdateFailed => "removal_update_failed",
            Self::RetainedIntentionally => "retained_intentionally",
        }
    }
}

impl fmt::Display for SubscriptionCleanupOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl LabelValue for SubscriptionCleanupOutcome {
    fn value(&self) -> &str {
        self.as_str()
    }
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
