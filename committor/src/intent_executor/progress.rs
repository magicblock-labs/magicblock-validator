use solana_signature::Signature;

use super::error::IntentExecutorError;

/// Progress of the current settlement transaction, independent of its tasks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TransactionProgress {
    #[default]
    Preparing,
    Prepared(Signature),
    Submitted(Signature),
    Rejected(Signature),
    Confirmed(Signature),
}

impl TransactionProgress {
    fn signature(self) -> Option<Signature> {
        match self {
            Self::Preparing => None,
            Self::Prepared(signature)
            | Self::Submitted(signature)
            | Self::Rejected(signature)
            | Self::Confirmed(signature) => Some(signature),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExecutionProgress {
    pub confirmed_commit: Option<Signature>,
    pub transaction: TransactionProgress,
}

impl ExecutionProgress {
    /// A fresh attempt must not repeat confirmed work or an unresolved submission.
    pub fn can_retry(self) -> bool {
        self.confirmed_commit.is_none()
            && !matches!(
                self.transaction,
                TransactionProgress::Submitted(_) | TransactionProgress::Confirmed(_)
            )
    }

    /// Known signatures in execution order, including a preparation/recovery
    /// transaction exposed by the error. Each signature appears only once.
    pub fn signatures(
        self,
        error: Option<&IntentExecutorError>,
    ) -> impl Iterator<Item = Signature> {
        let committed = self.confirmed_commit;
        let current =
            self.transaction.signature().filter(|signature| Some(*signature) != committed);
        let failed = error
            .and_then(IntentExecutorError::signature)
            .filter(|signature| Some(*signature) != committed && Some(*signature) != current);
        [committed, current, failed].into_iter().flatten()
    }
}
