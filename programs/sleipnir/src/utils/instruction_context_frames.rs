use solana_sdk::{
    instruction::InstructionError,
    pubkey::Pubkey,
    transaction_context::{InstructionContext, TransactionContext},
};

pub(crate) trait InstructionFrame {
    fn get_nesting_level(&self) -> usize;
    fn get_program_id(&self) -> Option<&Pubkey>;
}

pub(crate) struct InstructionContextFrame<'a> {
    instruction_ctx: &'a InstructionContext,
    transaction_ctx: &'a TransactionContext,
}

impl InstructionFrame for InstructionContextFrame<'_> {
    fn get_nesting_level(&self) -> usize {
        // NOTE: stack_height is nesting_level + 1 (1 based)
        self.instruction_ctx.get_stack_height() - 1
    }
    fn get_program_id(&self) -> Option<&Pubkey> {
        self.instruction_ctx
            .get_last_program_key(self.transaction_ctx)
            .ok()
    }
}

/// Represents all frames in a transaction in the order that they would be called.
/// For top level instrucions the nesting_level is 0.
/// All nested instructions are invoked via CPI.
///
/// The frames are in depth first order, meaning sibling instructions come behind
/// any child instrucions of a specific frame.
///
/// Thus this vec can hold frames of multiple stacks.
pub(crate) struct GenericInstructionContextFrames<F: InstructionFrame>(Vec<F>);

pub(crate) type InstructionContextFrames<'a> =
    GenericInstructionContextFrames<InstructionContextFrame<'a>>;

impl<F: InstructionFrame> GenericInstructionContextFrames<F> {
    pub(crate) fn new(frames: Vec<F>) -> Self {
        Self(frames)
    }

    pub fn find_parent_frame(&self, current_frame: &F) -> Option<&F> {
        let current_nesting_level = current_frame.get_nesting_level();
        if current_nesting_level == 0 {
            return None;
        }

        // Find the first frame whose nesting level is less than the current frame
        for frame_idx in (0..current_nesting_level).rev() {
            let frame = &self.0[frame_idx];
            let nesting_level = frame.get_nesting_level();
            if nesting_level < current_nesting_level {
                debug_assert_eq!(
                    nesting_level + 1,
                    current_nesting_level,
                    "cannot skip a frame"
                );
                return Some(frame);
            }
        }

        None
    }

    pub fn find_program_id_of_parent_frame(
        &self,
        current_frame: &F,
    ) -> Option<&Pubkey> {
        self.find_parent_frame(current_frame)
            .and_then(|frame| frame.get_program_id())
    }
}

impl<'a> InstructionContextFrames<'a> {
    pub fn find_program_id_of_parent_frame_from_ix_ctx(
        &'a self,
        instruction_ctx: &'a InstructionContext,
        transaction_ctx: &'a TransactionContext,
    ) -> Option<&Pubkey> {
        let current_frame = InstructionContextFrame {
            instruction_ctx,
            transaction_ctx,
        };
        self.find_program_id_of_parent_frame(&current_frame)
    }
}

impl<'a> TryFrom<&'a TransactionContext>
    for GenericInstructionContextFrames<InstructionContextFrame<'a>>
{
    type Error = InstructionError;
    fn try_from(ctx: &'a TransactionContext) -> Result<Self, Self::Error> {
        let mut frames = vec![];
        for idx in 0..ctx.get_instruction_trace_length() {
            let frame = ctx.get_instruction_context_at_index_in_trace(idx)?;
            frames.push(InstructionContextFrame {
                instruction_ctx: frame,
                transaction_ctx: ctx,
            });
        }

        Ok(GenericInstructionContextFrames::new(frames))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_parent_instruction() {}
}
