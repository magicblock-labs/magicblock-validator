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
