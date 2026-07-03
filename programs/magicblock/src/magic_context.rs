use std::{io::Cursor, mem};

use magicblock_core::{
    token_programs::{TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID},
    Slot,
};
use magicblock_magic_program_api::MAGIC_CONTEXT_SIZE;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use solana_hash::Hash;
use solana_instruction::error::InstructionError;
use solana_pubkey::Pubkey;
use solana_transaction::Transaction;

use crate::magic_scheduled_base_intent::{
    BaseAction, CommitAndUndelegate, CommitType, MagicIntentBundle,
    ScheduledIntentBundle,
};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct MagicContext {
    pub intent_id: u64,
    pub scheduled_base_intents: Vec<ScheduledIntentBundle>,
}

impl MagicContext {
    pub const SIZE: usize = MAGIC_CONTEXT_SIZE;
    pub const ZERO: [u8; Self::SIZE] = [0; Self::SIZE];

    pub(crate) fn deserialize(data: &[u8]) -> Result<Self, bincode::Error> {
        if data.is_empty() || is_zeroed(data) {
            Ok(Self::default())
        } else {
            match deserialize_with_zero_padding(data) {
                Ok(context) if has_valid_rent_pending_metadata(&context) => {
                    Ok(context)
                }
                Ok(_) => Self::deserialize_legacy(data),
                Err(err) => Self::deserialize_legacy(data).map_err(|_| err),
            }
        }
    }

    fn deserialize_legacy(data: &[u8]) -> Result<Self, bincode::Error> {
        deserialize_with_zero_padding::<LegacyMagicContext>(data)
            .map(Into::into)
    }

    pub(crate) fn write_to(
        &self,
        data: &mut [u8],
    ) -> Result<(), InstructionError> {
        let size = bincode::serialized_size(self)
            .map_err(|_| InstructionError::GenericError)?;
        if size > data.len() as u64 {
            return Err(InstructionError::AccountDataTooSmall);
        }

        data.fill(0);
        bincode::serialize_into(&mut &mut data[..], self)
            .map_err(|_| InstructionError::GenericError)
    }

    pub(crate) fn next_intent_id(&mut self) -> u64 {
        let output = self.intent_id;
        self.intent_id = self.intent_id.wrapping_add(1);

        output
    }

    pub(crate) fn add_scheduled_action(
        &mut self,
        base_intent: ScheduledIntentBundle,
    ) {
        self.scheduled_base_intents.push(base_intent);
    }

    pub(crate) fn take_scheduled_commits(
        &mut self,
    ) -> Vec<ScheduledIntentBundle> {
        mem::take(&mut self.scheduled_base_intents)
    }

    pub fn has_scheduled_commits(data: &[u8]) -> bool {
        const LEN_OFF: usize = mem::size_of::<u64>();
        const LEN_END: usize = LEN_OFF + mem::size_of::<u64>();

        if is_zeroed(data) {
            return false;
        }

        let Some(raw_len) = data.get(LEN_OFF..LEN_END) else {
            return false;
        };
        let mut len = [0; mem::size_of::<u64>()];
        len.copy_from_slice(raw_len);
        u64::from_le_bytes(len) != 0
    }
}

fn is_zeroed(buf: &[u8]) -> bool {
    const VEC_SIZE_LEN: usize = 8;
    const ZEROS: [u8; VEC_SIZE_LEN] = [0; VEC_SIZE_LEN];
    let mut chunks = buf.chunks_exact(VEC_SIZE_LEN);

    #[allow(clippy::indexing_slicing)]
    {
        chunks.all(|chunk| chunk == &ZEROS[..])
            && chunks.remainder() == &ZEROS[..chunks.remainder().len()]
    }
}

fn deserialize_with_zero_padding<T: DeserializeOwned + Serialize>(
    data: &[u8],
) -> Result<T, bincode::Error> {
    let value = bincode::deserialize_from(Cursor::new(data))?;
    let encoded = bincode::serialize(&value)?;
    if data.get(..encoded.len()) == Some(encoded.as_slice())
        && data.get(encoded.len()..).is_some_and(is_zeroed)
    {
        Ok(value)
    } else {
        Err(Box::new(bincode::ErrorKind::Custom(
            "non-zero trailing bytes".to_string(),
        )))
    }
}

fn has_valid_rent_pending_metadata(context: &MagicContext) -> bool {
    context.scheduled_base_intents.iter().all(|intent| {
        intent
            .intent_bundle
            .rent_pending_ata_materializations
            .iter()
            .all(|materialization| {
                materialization.ata_pubkey != Pubkey::default()
                    && materialization.eata_pubkey != Pubkey::default()
                    && materialization.wallet_owner != Pubkey::default()
                    && materialization.mint != Pubkey::default()
                    && materialization.validator != Pubkey::default()
                    && materialization.delegated_payer != Pubkey::default()
                    && materialization.delegated_vault != Pubkey::default()
                    && materialization.token_account_data_len != 0
                    && (materialization.token_program == TOKEN_PROGRAM_ID
                        || materialization.token_program
                            == TOKEN_2022_PROGRAM_ID)
            })
    })
}

// Compat invariant: pre-rent-pending MagicContext bytes do not contain
// `rent_pending_ata_materializations`; restored legacy intents use an empty vec.
#[derive(Serialize, Deserialize)]
struct LegacyMagicContext {
    intent_id: u64,
    scheduled_base_intents: Vec<LegacyScheduledIntentBundle>,
}

#[derive(Serialize, Deserialize)]
struct LegacyScheduledIntentBundle {
    id: u64,
    slot: Slot,
    blockhash: Hash,
    sent_transaction: Transaction,
    payer: Pubkey,
    intent_bundle: LegacyMagicIntentBundle,
}

#[derive(Serialize, Deserialize)]
struct LegacyMagicIntentBundle {
    commit: Option<CommitType>,
    commit_and_undelegate: Option<CommitAndUndelegate>,
    commit_finalize: Option<CommitType>,
    commit_finalize_and_undelegate: Option<CommitAndUndelegate>,
    standalone_actions: Vec<BaseAction>,
}

impl From<LegacyMagicContext> for MagicContext {
    fn from(value: LegacyMagicContext) -> Self {
        Self {
            intent_id: value.intent_id,
            scheduled_base_intents: value
                .scheduled_base_intents
                .into_iter()
                .map(Into::into)
                .collect(),
        }
    }
}

impl From<LegacyScheduledIntentBundle> for ScheduledIntentBundle {
    fn from(value: LegacyScheduledIntentBundle) -> Self {
        Self {
            id: value.id,
            slot: value.slot,
            blockhash: value.blockhash,
            sent_transaction: value.sent_transaction,
            payer: value.payer,
            intent_bundle: value.intent_bundle.into(),
        }
    }
}

impl From<LegacyMagicIntentBundle> for MagicIntentBundle {
    fn from(value: LegacyMagicIntentBundle) -> Self {
        Self {
            commit: value.commit,
            commit_and_undelegate: value.commit_and_undelegate,
            commit_finalize: value.commit_finalize,
            commit_finalize_and_undelegate: value
                .commit_finalize_and_undelegate,
            standalone_actions: value.standalone_actions,
            rent_pending_ata_materializations: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use solana_hash::Hash;
    use solana_pubkey::Pubkey;
    use solana_transaction::Transaction;

    use super::{
        LegacyMagicContext, LegacyMagicIntentBundle,
        LegacyScheduledIntentBundle, MagicContext,
    };

    #[test]
    fn deserialize_treats_empty_and_zeroed_as_default() {
        let zero = vec![0; MagicContext::SIZE];

        assert_eq!(MagicContext::deserialize(&[]).unwrap().intent_id, 0);
        assert_eq!(MagicContext::deserialize(&zero).unwrap().intent_id, 0);
    }

    #[test]
    fn deserialize_ignores_trailing_zero_padding() {
        let ctx = MagicContext {
            intent_id: 7,
            scheduled_base_intents: Vec::new(),
        };
        let mut data = vec![0; MagicContext::SIZE];

        ctx.write_to(&mut data).unwrap();

        let got = MagicContext::deserialize(&data).unwrap();
        assert_eq!(got.intent_id, ctx.intent_id);
        assert!(got.scheduled_base_intents.is_empty());
    }

    #[test]
    fn deserialize_recovers_legacy_context_with_two_pending_intents() {
        let legacy = LegacyMagicContext {
            intent_id: 11,
            scheduled_base_intents: vec![
                LegacyScheduledIntentBundle {
                    id: 1,
                    slot: 2,
                    blockhash: Hash::new_unique(),
                    sent_transaction: Transaction::default(),
                    payer: Pubkey::new_unique(),
                    intent_bundle: LegacyMagicIntentBundle {
                        commit: None,
                        commit_and_undelegate: None,
                        commit_finalize: None,
                        commit_finalize_and_undelegate: None,
                        standalone_actions: Vec::new(),
                    },
                },
                LegacyScheduledIntentBundle {
                    id: 999,
                    slot: 3,
                    blockhash: Hash::new_unique(),
                    sent_transaction: Transaction::default(),
                    payer: Pubkey::new_unique(),
                    intent_bundle: LegacyMagicIntentBundle {
                        commit: None,
                        commit_and_undelegate: None,
                        commit_finalize: None,
                        commit_finalize_and_undelegate: None,
                        standalone_actions: Vec::new(),
                    },
                },
            ],
        };
        let encoded = bincode::serialize(&legacy).unwrap();
        let mut data = vec![0; MagicContext::SIZE];
        data[..encoded.len()].copy_from_slice(&encoded);

        let got = MagicContext::deserialize(&data).unwrap();

        assert_eq!(got.intent_id, 11);
        assert_eq!(got.scheduled_base_intents.len(), 2);
        assert_eq!(got.scheduled_base_intents[0].id, 1);
        assert_eq!(got.scheduled_base_intents[1].id, 999);
        assert!(got.scheduled_base_intents.iter().all(|intent| {
            intent
                .intent_bundle
                .rent_pending_ata_materializations
                .is_empty()
        }));
    }
}
