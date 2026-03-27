use serde::{Deserialize, Serialize};
use solana_signature::Signature;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MagicResponse {
    V1(MagicResponseV1),
}

impl MagicResponse {
    pub fn ok(&self) -> bool {
        match self {
            Self::V1(value) => value.ok,
        }
    }

    pub fn data(&self) -> &[u8] {
        match self {
            Self::V1(value) => &value.data,
        }
    }

    pub fn error(&self) -> &String {
        match self {
            Self::V1(value) => &value.error,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MagicResponseV1 {
    pub ok: bool,
    /// Data user specified as payload for callback
    /// Present even in case of an error
    pub data: Vec<u8>,
    /// Reason for callback execution with ok = false
    /// TimeoutError/ActionError
    pub error: String,
    /// Action execution receipt entries
    /// Present if signature of action tx is available
    pub receipt: Option<ActionReceipt>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionReceipt {
    /// action signature
    pub signature: Signature,
}
