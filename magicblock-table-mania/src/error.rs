use solana_pubkey::Pubkey;
use solana_sdk::signature::Signature;
use thiserror::Error;

pub type TableManiaResult<T> = std::result::Result<T, TableManiaError>;

#[derive(Error, Debug)]
pub enum TableManiaError {
    #[error("MagicBlockRpcClientError: {0} ({0:?})")]
    MagicBlockRpcClientError(
        #[from] magicblock_rpc_client::MagicBlockRpcClientError,
    ),

    #[error("Cannot extend deactivated table {0}.")]
    CannotExtendDeactivatedTable(Pubkey),

    #[error("Can only use one authority for a TableMania instance. {0} does not match {1}.")]
    InvalidAuthority(Pubkey, Pubkey),

    #[error("Can only extend by {0} pubkeys at a time, but was provided {1}")]
    MaxExtendPubkeysExceeded(usize, usize),

    #[error("Timed out waiting for remote tables to update: {0}")]
    TimedOutWaitingForRemoteTablesToUpdate(String),

    #[error("Timed out waiting for local tables to update: {0}")]
    TimedOutWaitingForLocalTablesToUpdate(String),
}

impl TableManiaError {
    /// Returns a signature related to this error if available.
    pub fn signature(&self) -> Option<Signature> {
        match self {
            TableManiaError::MagicBlockRpcClientError(err) => err.signature(),
            _ => None,
        }
    }
}
