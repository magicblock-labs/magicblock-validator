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

impl AccountFetchOrigin {
    pub fn as_str(&self) -> &str {
        use AccountFetchOrigin::*;
        match self {
            GetMultipleAccounts => "get_multiple_accounts",
            GetAccount => "get_account",
            SendTransaction => "send_transaction",
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

// -----------------
// ProgramFetchResult
// -----------------
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgramFetchResult {
    Failed,
    Found,
    NotFound,
}

impl ProgramFetchResult {
    pub fn as_str(&self) -> &str {
        use ProgramFetchResult::*;
        match self {
            Failed => "failed",
            Found => "found",
            NotFound => "not_found",
        }
    }
}

impl fmt::Display for ProgramFetchResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl LabelValue for ProgramFetchResult {
    fn value(&self) -> &str {
        self.as_str()
    }
}

// -----------------
// ProgramAlias
// -----------------
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgramAlias {
    Token,
    Magic,
    Serum,
    Raydium,
    Marinade,
    Orca,
    JupiterAg,
    Unknown,
}

impl ProgramAlias {
    pub fn as_str(&self) -> &str {
        use ProgramAlias::*;
        match self {
            Token => "token",
            Magic => "magic",
            Serum => "serum",
            Raydium => "raydium",
            Marinade => "marinade",
            Orca => "orca",
            JupiterAg => "jupiter",
            Unknown => "unknown",
        }
    }

    /// Maps a program pubkey to a known program alias.
    /// Known programs are matched by their canonical addresses.
    pub fn from_program_id(program_id: &str) -> Self {
        use ProgramAlias::*;
        match program_id {
            // Token program
            "TokenkegQfeZyiNwAJsyFbPVwwQQforrMsqhtQTgC1" => Token,
            // Magic program - placeholder, replace with actual
            "magicEDQB5KqNAqkL87j3xfyduESdcrJnvJB3nj2N7f" => Magic,
            // Serum DEX program
            "9xQeWvG816bUx9EPjHmaT23EB3rLT6shESFG3PvmUpS" => Serum,
            // Raydium program
            "675kPX9MHTjS2zt1qLCncYAuYpNbY4dQLQSleq5bLfV" => Raydium,
            // Marinade Finance program
            "MarBmsSgKXdrQP1zDtKfSnLYMoC5r76z6PMoEEujkXi" => Marinade,
            // Orca program
            "whirLbMiicVdio4KfQ7N0xrKDZQxC7J5iD2DtxSEP3" => Orca,
            // Jupiter AG program
            "JUP6LkbZbjS1jKKB3QtvJYUQTRzPrAG69omh2CEYqAc" => JupiterAg,
            _ => Unknown,
        }
    }
}

impl fmt::Display for ProgramAlias {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl LabelValue for ProgramAlias {
    fn value(&self) -> &str {
        self.as_str()
    }
}
