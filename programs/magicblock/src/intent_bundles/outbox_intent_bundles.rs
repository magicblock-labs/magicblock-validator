use magicblock_core::intent::outbox::OUTBOX_INTENT_DISCRIMINATOR;
use serde::{Deserialize, Serialize};
use solana_signature::Signature;

use crate::magic_scheduled_base_intent::ScheduledIntentBundle;

#[derive(Debug, Serialize, Deserialize)]
pub struct OutboxIntentBundle {
    pub inner: ScheduledIntentBundle,
    pub status: OutboxIntentBundleStatus,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum OutboxIntentBundleStatus {
    Accepted,
    Executing(ExecutionStage),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ExecutionStage {
    SingleStage(Signature),
    TwoStage(TwoStageProgress),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TwoStageProgress {
    Committing(Signature),
    Finalizing {
        commit: Signature,
        finalize: Signature,
    },
}

impl OutboxIntentBundle {
    pub fn accepted(intent_bundle: ScheduledIntentBundle) -> Self {
        Self {
            inner: intent_bundle,
            status: OutboxIntentBundleStatus::Accepted,
        }
    }

    pub fn try_to_bytes(&self) -> Result<Vec<u8>, bincode::Error> {
        let body = bincode::serialize(self)?;
        let mut out =
            Vec::with_capacity(OUTBOX_INTENT_DISCRIMINATOR.len() + body.len());
        out.extend_from_slice(&OUTBOX_INTENT_DISCRIMINATOR);
        out.extend_from_slice(&body);
        Ok(out)
    }

    pub fn try_from_bytes(data: &[u8]) -> Result<Self, bincode::Error> {
        let disc_len = OUTBOX_INTENT_DISCRIMINATOR.len();
        if data.len() < disc_len
            || data[..disc_len] != OUTBOX_INTENT_DISCRIMINATOR
        {
            return Err(Box::new(bincode::ErrorKind::Custom(
                "invalid discriminator".into(),
            )));
        }
        bincode::deserialize(&data[disc_len..])
    }
}
