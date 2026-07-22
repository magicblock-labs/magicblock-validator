use std::ops::Deref;

use magicblock_core::intent::outbox::OUTBOX_INTENT_DISCRIMINATOR;
use magicblock_magic_program_api::outbox::{
    ExecutionStage, PendingTransaction, TwoStageProgress,
};
use serde::{Deserialize, Serialize};
use solana_hash::Hash;
use solana_signature::Signature;

use crate::magic_scheduled_base_intent::ScheduledIntentBundle;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutboxIntentBundle {
    pub inner: ScheduledIntentBundle,
    status: OutboxIntentBundleStatus,
}

impl OutboxIntentBundle {
    pub fn accepted(intent_bundle: ScheduledIntentBundle) -> Self {
        Self {
            inner: intent_bundle,
            status: OutboxIntentBundleStatus::Accepted,
        }
    }

    pub fn status(&self) -> &OutboxIntentBundleStatus {
        &self.status
    }

    pub(crate) fn apply_stage_transition(
        &mut self,
        stage: ExecutionStage,
    ) -> Result<(), &'static str> {
        self.status.apply_stage_transition(stage)
    }

    #[cfg(not(feature = "dev-context-only-utils"))]
    pub(crate) fn try_to_bytes(&self) -> Result<Vec<u8>, bincode::Error> {
        self.try_to_bytes_impl()
    }

    #[cfg(feature = "dev-context-only-utils")]
    pub fn try_to_bytes(&self) -> Result<Vec<u8>, bincode::Error> {
        self.try_to_bytes_impl()
    }

    fn try_to_bytes_impl(&self) -> Result<Vec<u8>, bincode::Error> {
        const DISCRIMINATOR_LEN: usize = OUTBOX_INTENT_DISCRIMINATOR.len();

        // bincode serializes structs as field concatenation, so max body size
        // is inner size + worst-case status size (TwoStage::Finalizing with 2 sigs)
        let max_body_size = (bincode::serialized_size(&self.inner)?
            + bincode::serialized_size(
                &OutboxIntentBundleStatus::max_size_variant(),
            )?) as usize;

        let mut out = vec![0u8; DISCRIMINATOR_LEN + max_body_size];
        out[..DISCRIMINATOR_LEN].copy_from_slice(&OUTBOX_INTENT_DISCRIMINATOR);
        bincode::serialize_into(
            std::io::Cursor::new(&mut out[DISCRIMINATOR_LEN..]),
            self,
        )?;
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
    fn max_size_variant() -> Self {
        Self::Executing(ExecutionStage::TwoStage(
            TwoStageProgress::Finalizing {
                commit: Signature::default(),
                finalize: PendingTransaction {
                    signature: Signature::default(),
                    blockhash: Hash::default(),
                },
            },
        ))
    }

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
