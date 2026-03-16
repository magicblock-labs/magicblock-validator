use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MagicResponse {
    pub ok: bool,
    /// Data user specified as payload for callback
    /// Present even in case of an error
    pub data: Vec<u8>,
    /// Reason for callback execution with ok = false
    /// TimoutError/ActionError
    pub error: String,
}
