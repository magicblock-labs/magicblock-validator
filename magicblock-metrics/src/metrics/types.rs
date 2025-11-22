use std::fmt;

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
    SendTransaction,
}

impl fmt::Display for AccountFetchOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AccountFetchOrigin::GetMultipleAccounts => {
                write!(f, "get_multiple_accounts")
            }
            AccountFetchOrigin::GetAccount => write!(f, "get_account"),
            AccountFetchOrigin::SendTransaction => {
                write!(f, "send_transaction")
            }
        }
    }
}

impl LabelValue for AccountFetchOrigin {
    fn value(&self) -> &str {
        use AccountFetchOrigin::*;
        match self {
            GetMultipleAccounts => "get_multiple_accounts",
            GetAccount => "get_account",
            SendTransaction => "send_transaction",
        }
    }
}

pub trait LabelValue {
    fn value(&self) -> &str;
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
