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
            Success => write!(f, "{}", OUTCOME_SUCCESS),
            Error => write!(f, "{}", OUTCOME_ERROR),
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
}

// -----------------
// AccountClone
// -----------------
pub enum AccountClone<'a> {
    Wallet { pubkey: &'a str },
    Undelegated { pubkey: &'a str, owner: &'a str },
    Delegated { pubkey: &'a str, owner: &'a str },
    Program { pubkey: &'a str },
}

// -----------------
// AccountCommit
// -----------------
pub enum AccountCommit<'a> {
    CommitOnly { pubkey: &'a str, outcome: Outcome },
    CommitAndUndelegate { pubkey: &'a str, outcome: Outcome },
}
