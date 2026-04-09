use std::{io::Cursor, mem};

use magicblock_magic_program_api::MAGIC_CONTEXT_SIZE;
use serde::{Deserialize, Serialize};
use solana_instruction::error::InstructionError;

use crate::magic_scheduled_base_intent::ScheduledIntentBundle;

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
            bincode::deserialize_from(Cursor::new(data))
        }
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

#[cfg(test)]
mod tests {
    use super::MagicContext;

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
}
