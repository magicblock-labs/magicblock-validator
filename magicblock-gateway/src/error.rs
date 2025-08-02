use std::{error::Error, fmt::Display};

use json::Serialize;

const TRANSACTION_SIMULATION: i16 = -32002;
const TRANSACTION_VERIFICATION: i16 = -32003;
const INVALID_REQUEST: i16 = -32600;
const METHOD_NOTFOUND: i16 = -32601;
const INVALID_PARAMS: i16 = -32602;
const INTERNAL_ERROR: i16 = -32603;
const PARSE_ERROR: i16 = -32700;

#[derive(Serialize, Debug)]
pub(crate) struct RpcError {
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

impl RpcError {
    pub(crate) fn invalid_params<E: Display>(error: E) -> Self {
        Self {
            code: INVALID_PARAMS,
            message: format!("invalid request params: {error}"),
        }
    }

    pub(crate) fn transaction_simulation<E: Error>(error: E) -> Self {
        Self {
            code: TRANSACTION_SIMULATION,
            message: error.to_string(),
        }
    }

    pub(crate) fn transaction_verification<E: Error>(error: E) -> Self {
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

    pub(crate) fn method_not_found<M: Display>(method: M) -> Self {
        Self {
            code: METHOD_NOTFOUND,
            message: format!("method not found: {method}"),
        }
    }

    pub(crate) fn parse_error<E: Error>(error: E) -> Self {
        Self {
            code: PARSE_ERROR,
            message: format!("error parsing request body: {error}"),
        }
    }

    pub(crate) fn internal<E: Error>(error: E) -> Self {
        Self {
            code: INTERNAL_ERROR,
            message: format!("internal server error: {error}"),
        }
    }
}
