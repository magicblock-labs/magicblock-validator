use std::mem;

use magicblock_magic_program_api::MAGIC_CONTEXT_SIZE;
use serde::{Deserialize, Serialize};
use solana_sdk::account::{AccountSharedData, ReadableAccount};

use crate::magic_scheduled_base_intent::ScheduledBaseIntent;

#[repr(C)]
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct MagicContext {
    pub intent_id: u64,
    pub scheduled_base_intents: Vec<ScheduledBaseIntent>,
}

impl MagicContext {
    pub const SIZE: usize = MAGIC_CONTEXT_SIZE;
    pub const ZERO: [u8; Self::SIZE] = [0; Self::SIZE];
    pub(crate) fn deserialize(
        data: &AccountSharedData,
    ) -> Result<Self, bincode::Error> {
        if data.data().is_empty() {
            Ok(Self::default())
        } else {
            data.deserialize_data()
        }
    }

    pub(crate) fn next_intent_id(&mut self) -> u64 {
        let output = self.intent_id;
        self.intent_id = self.intent_id.wrapping_add(1);

        output
    }

    pub(crate) fn add_scheduled_action(
        &mut self,
        base_intent: ScheduledBaseIntent,
    ) {
        self.scheduled_base_intents.push(base_intent);
    }

    pub(crate) fn take_scheduled_commits(
        &mut self,
    ) -> Vec<ScheduledBaseIntent> {
        mem::take(&mut self.scheduled_base_intents)
    }

    pub fn has_scheduled_commits(data: &[u8]) -> bool {
        // Currently we only store a vec of scheduled commits in the MagicContext
        // The first bytes 8..16 contain the length of the vec
        // This works even if the length is actually stored as a u32
        // since we zero out the entire context whenever we update the vec
        !is_zeroed(&data[8..16])
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
