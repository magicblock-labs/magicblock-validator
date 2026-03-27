use serde::{Deserialize, Serialize};
use solana_program::clock::Slot;
use solana_signature::Signature;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MagicResponse {
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
    /// TX
    pub signature: Signature,
    /// Slot if found. Always `None` for now.
    pub slot: Option<Slot>,
    /// Logs if found. Always `None` for now.
    pub logs: Option<String>,
}
