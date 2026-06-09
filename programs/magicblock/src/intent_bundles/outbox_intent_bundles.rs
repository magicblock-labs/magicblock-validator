use std::ops::Deref;

use magicblock_core::intent::outbox::OUTBOX_INTENT_DISCRIMINATOR;
use magicblock_magic_program_api::outbox::{ExecutionStage, TwoStageProgress};
use serde::{Deserialize, Serialize};

use crate::magic_scheduled_base_intent::ScheduledIntentBundle;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutboxIntentBundle {
    pub inner: ScheduledIntentBundle,
    pub status: OutboxIntentBundleStatus,
}

impl OutboxIntentBundle {
    pub fn accepted(intent_bundle: ScheduledIntentBundle) -> Self {
        Self {
            inner: intent_bundle,
            status: OutboxIntentBundleStatus::Accepted,
        }
    }

    pub fn apply_stage_transition(
        &mut self,
        stage: ExecutionStage,
    ) -> Result<(), &'static str> {
        self.status.apply_stage_transition(stage)
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum OutboxIntentBundleStatus {
    Accepted,
    Executing(ExecutionStage),
}

impl OutboxIntentBundleStatus {
    // TODO(edwin): split into is_valid_transition and apply_transaction
    fn apply_stage_transition(
        &mut self,
        stage: ExecutionStage,
    ) -> Result<(), &'static str> {
        match (self, stage) {
            (this @ Self::Accepted, ExecutionStage::TwoStage(stage)) => {
                match stage {
                    // Transition from Accepted to Committing stage
                    val @ TwoStageProgress::Committing(_) => {
                        *this = Self::Executing(ExecutionStage::TwoStage(val));
                    }
                    // Transition from Accepted state to TwoStage::Finalizing is invalid
                    TwoStageProgress::Finalizing { .. } => {
                        return Err(
                            "cannot transition from Accepted to Finalizing",
                        )
                    }
                }
            }
            (this @ Self::Accepted, val @ ExecutionStage::SingleStage(_)) => {
                *this = Self::Executing(val);
            }
            (Self::Executing(ref mut this), stage) => {
                this.apply_stage_transition(stage)?;
            }
        };
        Ok(())
    }
}

impl Deref for OutboxIntentBundle {
    type Target = ScheduledIntentBundle;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
