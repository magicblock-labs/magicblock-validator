use std::{error::Error, fmt::Display};

use json::Serialize;
use solana_transaction_error::TransactionError;

pub(crate) const TRANSACTION_SIMULATION: i16 = -32002;
pub(crate) const TRANSACTION_VERIFICATION: i16 = -32003;
pub(crate) const BLOCK_NOT_FOUND: i16 = -32009;
pub(crate) const INVALID_REQUEST: i16 = -32600;
pub(crate) const INVALID_PARAMS: i16 = -32602;
pub(crate) const INTERNAL_ERROR: i16 = -32603;
pub(crate) const PARSE_ERROR: i16 = -32700;

#[derive(Serialize, Debug)]
pub struct RpcError {
    code: i16,
    message: String,
}

impl Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "RPC Error. Code: {}. Message: {}",
            self.code, self.message
        )
    }
}

impl Error for RpcError {}

impl From<hyper::Error> for RpcError {
    fn from(value: hyper::Error) -> Self {
        Self::invalid_request(value)
    }
}

impl From<json::Error> for RpcError {
    fn from(value: json::Error) -> Self {
        Self::parse_error(value)
    }
}

impl From<TransactionError> for RpcError {
    fn from(value: TransactionError) -> Self {
        Self::transaction_verification(value)
    }
}

impl From<magicblock_ledger::errors::LedgerError> for RpcError {
    fn from(value: magicblock_ledger::errors::LedgerError) -> Self {
        Self::internal(value)
    }
}

impl From<magicblock_accounts_db::error::AccountsDbError> for RpcError {
    fn from(value: magicblock_accounts_db::error::AccountsDbError) -> Self {
        Self::internal(value)
    }
}

#[macro_export]
macro_rules! some_or_err {
    ($val: ident) => {
        some_or_err!($val, stringify!($val))
    };
    ($val: expr, $label: expr) => {
        $val.map(Into::into).ok_or_else(|| {
            $crate::error::RpcError::invalid_params(concat!(
                "missing or invalid ",
                $label
            ))
        })?
    };
}

impl RpcError {
    pub(crate) fn invalid_params<E: Display>(error: E) -> Self {
        Self {
            code: INVALID_PARAMS,
            message: format!("invalid request params: {error}"),
        }
    }

    pub(crate) fn transaction_simulation<E: Display>(error: E) -> Self {
        Self {
            code: TRANSACTION_SIMULATION,
            message: error.to_string(),
        }
    }

    pub(crate) fn transaction_verification<E: Display>(error: E) -> Self {
        Self {
            code: TRANSACTION_VERIFICATION,
            message: format!("transaction verification error: {error}"),
        }
    }

    pub(crate) fn invalid_request<E: Display>(error: E) -> Self {
        Self {
            code: INVALID_REQUEST,
            message: format!("invalid request: {error}"),
        }
    }

    pub(crate) fn parse_error<E: Error>(error: E) -> Self {
        Self {
            code: PARSE_ERROR,
            message: format!("error parsing request body: {error}"),
        }
    }

    pub(crate) fn internal<E: Display>(error: E) -> Self {
        Self {
            code: INTERNAL_ERROR,
            message: format!("internal server error: {error}"),
        }
    }

    pub(crate) fn custom<E: Display>(error: E, code: i16) -> Self {
        Self {
            code,
            message: error.to_string(),
        }
    }
}
