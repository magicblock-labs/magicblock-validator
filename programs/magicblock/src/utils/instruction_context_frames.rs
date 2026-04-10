use solana_instruction::error::InstructionError;
use solana_pubkey::Pubkey;
use solana_transaction_context::TransactionContext;

pub(crate) trait InstructionFrame {
    /// How deeply in the stack this frame is.
    fn get_nesting_level(&self) -> usize;
    /// At which index of all frames in a frames collection this frame is.
    fn get_index(&self) -> usize;
    /// The program id of the frame.
    fn get_program_id(&self) -> Option<&Pubkey>;
}

pub(crate) struct InstructionContextFrame {
    nesting_level: usize,
    program_id: Option<Pubkey>,
    index: usize,
}

impl InstructionFrame for InstructionContextFrame {
    fn get_nesting_level(&self) -> usize {
        self.nesting_level
    }

    fn get_index(&self) -> usize {
        self.index
    }

    fn get_program_id(&self) -> Option<&Pubkey> {
        self.program_id.as_ref()
    }
}

/// Represents all frames in a transaction in the order that they would be called.
/// For top level instructions the nesting_level is 0.
/// All nested instructions are invoked via CPI.
///
/// This vec holds frames of multiple stacks.
///
/// The frames are in depth first order, meaning sibling instructions come after
/// any child instructions of a specific frame.
pub(crate) struct GenericInstructionContextFrames<F: InstructionFrame>(Vec<F>);

impl<F: InstructionFrame> GenericInstructionContextFrames<F> {
    pub(crate) fn new(frames: Vec<F>) -> Self {
        Self(frames)
    }

    pub fn find_parent_frame(&self, current_frame: &F) -> Option<&F> {
        let current_nesting_level = current_frame.get_nesting_level();
        if current_nesting_level == 0 {
            return None;
        }
        let current_index = current_frame.get_index();

        for frame_idx in (0..current_index).rev() {
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

    #[cfg(test)]
    fn get(&self, idx: usize) -> &F {
        &self.0[idx]
    }
}

pub(crate) struct InstructionContextFrames {
    frames: GenericInstructionContextFrames<InstructionContextFrame>,
    current_frame_idx: usize,
}

impl InstructionContextFrames {
    pub fn new(
        frames: Vec<InstructionContextFrame>,
        current_frame_idx: usize,
    ) -> Self {
        Self {
            frames: GenericInstructionContextFrames::new(frames),
            current_frame_idx,
        }
    }

    pub fn find_program_id_of_parent_of_current_instruction(
        &self,
    ) -> Option<&Pubkey> {
        self.frames
            .find_program_id_of_parent_frame(self.current_frame())
    }

    fn current_frame(&self) -> &InstructionContextFrame {
        &self.frames.0[self.current_frame_idx]
    }
}

impl TryFrom<&TransactionContext<'_>> for InstructionContextFrames {
    type Error = InstructionError;

    fn try_from(ctx: &TransactionContext<'_>) -> Result<Self, Self::Error> {
        let current_ix_ctx = ctx.get_current_instruction_context()?;
        let current_ix_idx = current_ix_ctx.get_index_in_trace();
        let mut current_frame_idx = None;
        let mut frames = Vec::with_capacity(ctx.get_instruction_trace_length());

        for idx in 0..ctx.get_instruction_trace_length() {
            let ix_ctx = ctx.get_instruction_context_at_index_in_trace(idx)?;
            if current_ix_idx == idx {
                current_frame_idx = Some(idx);
            }
            let nesting_level = ix_ctx
                .get_stack_height()
                .checked_sub(1)
                .ok_or(InstructionError::InvalidAccountData)?;
            frames.push(InstructionContextFrame {
                nesting_level,
                program_id: ix_ctx.get_program_key().ok().copied(),
                index: idx,
            });
        }

        let current_frame_idx =
            current_frame_idx.ok_or(InstructionError::InvalidAccountData)?;
        Ok(Self::new(frames, current_frame_idx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct InstructionFrameStub {
        nesting_level: usize,
        program_id: Option<Pubkey>,
        index: usize,
    }

    impl InstructionFrame for InstructionFrameStub {
        fn get_nesting_level(&self) -> usize {
            self.nesting_level
        }
        fn get_program_id(&self) -> Option<&Pubkey> {
            self.program_id.as_ref()
        }
        fn get_index(&self) -> usize {
            self.index
        }
    }

    fn setup_frames(
        frames: Vec<InstructionFrameStub>,
    ) -> GenericInstructionContextFrames<InstructionFrameStub> {
        GenericInstructionContextFrames::<InstructionFrameStub>::new(frames)
    }

    #[test]
    fn find_parent_program_empty_frames() {
        let frames = setup_frames(vec![]);
        let frame = InstructionFrameStub {
            nesting_level: 0,
            program_id: None,
            index: 0,
        };
        assert_eq!(frames.find_program_id_of_parent_frame(&frame), None);
    }

    #[test]
    fn find_parent_of_nested_instruction() {
        let program_id = Pubkey::new_unique();
        let frames = setup_frames(vec![
            InstructionFrameStub {
                nesting_level: 0,
                program_id: Some(program_id),
                index: 0,
            },
            InstructionFrameStub {
                nesting_level: 1,
                program_id: Some(Pubkey::new_unique()),
                index: 1,
            },
        ]);
        assert_eq!(frames.find_program_id_of_parent_frame(frames.get(0)), None,);
        assert_eq!(
            frames.find_program_id_of_parent_frame(frames.get(1)),
            Some(program_id).as_ref()
        );
    }

    #[test]
    fn find_parent_in_large_deeply_nexted_instructions() {
        let program_id_uno = Pubkey::new_unique();
        let program_id_dos = Pubkey::new_unique();
        let program_id_tres = Pubkey::new_unique();
        let program_id_cuatro = Pubkey::new_unique();
        let program_id_cinco = Pubkey::new_unique();
        let program_id_seis = Pubkey::new_unique();
        let program_id_siete = Pubkey::new_unique();
        let program_id_ocho = Pubkey::new_unique();
        let program_id_nueve = Pubkey::new_unique();

        /*
        +Uno
            -> Dos
                -> Tres
        +Cuatro
            -> Cinco
                -> Seis
        +Siete
        +Ocho
            -> Nueve
        */
        let frames = setup_frames(vec![
            InstructionFrameStub {
                nesting_level: 0,
                program_id: Some(program_id_uno),
                index: 0,
            },
            InstructionFrameStub {
                nesting_level: 1,
                program_id: Some(program_id_dos),
                index: 1,
            },
            InstructionFrameStub {
                nesting_level: 2,
                program_id: Some(program_id_tres),
                index: 2,
            },
            InstructionFrameStub {
                nesting_level: 0,
                program_id: Some(program_id_cuatro),
                index: 3,
            },
            InstructionFrameStub {
                nesting_level: 1,
                program_id: Some(program_id_cinco),
                index: 4,
            },
            InstructionFrameStub {
                nesting_level: 2,
                program_id: Some(program_id_seis),
                index: 5,
            },
            InstructionFrameStub {
                nesting_level: 0,
                program_id: Some(program_id_siete),
                index: 6,
            },
            InstructionFrameStub {
                nesting_level: 0,
                program_id: Some(program_id_ocho),
                index: 7,
            },
            InstructionFrameStub {
                nesting_level: 1,
                program_id: Some(program_id_nueve),
                index: 8,
            },
        ]);

        assert_eq!(frames.find_program_id_of_parent_frame(frames.get(0)), None,);
        assert_eq!(
            frames.find_program_id_of_parent_frame(frames.get(1)),
            Some(program_id_uno).as_ref()
        );
        assert_eq!(
            frames.find_program_id_of_parent_frame(frames.get(2)),
            Some(program_id_dos).as_ref()
        );
        assert_eq!(frames.find_program_id_of_parent_frame(frames.get(3)), None,);
        assert_eq!(
            frames.find_program_id_of_parent_frame(frames.get(4)),
            Some(program_id_cuatro).as_ref()
        );
        assert_eq!(
            frames.find_program_id_of_parent_frame(frames.get(5)),
            Some(program_id_cinco).as_ref()
        );
        assert_eq!(frames.find_program_id_of_parent_frame(frames.get(6)), None,);
        assert_eq!(frames.find_program_id_of_parent_frame(frames.get(7)), None,);
        assert_eq!(
            frames.find_program_id_of_parent_frame(frames.get(8)),
            Some(program_id_ocho).as_ref()
        );
    }
}
