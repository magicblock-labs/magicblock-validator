use solana_pubkey::Pubkey;
use solana_signature::Signature;
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

    pub fn is_sent_transaction_invalid_instruction_data_at(
        &self,
        instruction_index: u8,
    ) -> bool {
        use magicblock_rpc_client::MagicBlockRpcClientError;
        use solana_instruction::error::InstructionError;
        use solana_transaction_error::TransactionError;

        match self {
            TableManiaError::MagicBlockRpcClientError(
                MagicBlockRpcClientError::SentTransactionError(
                    TransactionError::InstructionError(
                        index,
                        InstructionError::InvalidInstructionData,
                    ),
                    _,
                ),
            ) => *index == instruction_index,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use magicblock_rpc_client::MagicBlockRpcClientError;
    use solana_instruction::error::InstructionError;
    use solana_signature::Signature;
    use solana_transaction_error::TransactionError;

    use super::TableManiaError;

    #[test]
    fn classifies_sent_transaction_invalid_instruction_data_at_index() {
        let err = TableManiaError::MagicBlockRpcClientError(
            MagicBlockRpcClientError::SentTransactionError(
                TransactionError::InstructionError(
                    2,
                    InstructionError::InvalidInstructionData,
                ),
                Signature::default(),
            ),
        );

        assert!(err.is_sent_transaction_invalid_instruction_data_at(2));
        assert!(!err.is_sent_transaction_invalid_instruction_data_at(1));
    }

    #[test]
    fn does_not_classify_other_sent_transaction_errors() {
        let err = TableManiaError::MagicBlockRpcClientError(
            MagicBlockRpcClientError::SentTransactionError(
                TransactionError::InstructionError(
                    2,
                    InstructionError::InvalidArgument,
                ),
                Signature::default(),
            ),
        );

        assert!(!err.is_sent_transaction_invalid_instruction_data_at(2));
    }
}
