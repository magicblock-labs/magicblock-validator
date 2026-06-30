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
