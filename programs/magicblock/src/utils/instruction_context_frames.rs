use solana_instruction::error::InstructionError;
use solana_pubkey::Pubkey;
use solana_transaction_context::TransactionContext;

/// The native caller link is authoritative: trace indices are not execution order.
pub(crate) struct InstructionContextFrames {
    parent_program_id: Option<Pubkey>,
}

impl InstructionContextFrames {
    pub fn find_program_id_of_parent_of_current_instruction(
        &self,
    ) -> Option<&Pubkey> {
        self.parent_program_id.as_ref()
    }
}

impl TryFrom<&TransactionContext<'_>> for InstructionContextFrames {
    type Error = InstructionError;

    fn try_from(ctx: &TransactionContext<'_>) -> Result<Self, Self::Error> {
        let caller =
            ctx.get_current_instruction_context()?.get_index_of_caller();
        let parent_program_id = if caller == usize::from(u16::MAX) {
            None
        } else {
            Some(
                *ctx.get_instruction_context_at_index_in_trace(caller)?
                    .get_program_key()?,
            )
        };
        Ok(Self { parent_program_id })
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use solana_account::AccountSharedData;
    use solana_sdk_ids::native_loader;
    use solana_transaction_context::MAX_ACCOUNTS_PER_TRANSACTION;

    use super::*;

    fn context(
        programs: &[Pubkey],
        top_level: usize,
    ) -> TransactionContext<'static> {
        let mut ctx = TransactionContext::new(
            programs
                .iter()
                .map(|key| {
                    (*key, AccountSharedData::new(0, 0, &native_loader::ID))
                })
                .collect(),
            Default::default(),
            10,
            20,
            top_level,
        );
        for index in 0..top_level {
            ctx.configure_instruction_at_index(
                index,
                index as u16,
                vec![],
                vec![u16::MAX; MAX_ACCOUNTS_PER_TRANSACTION],
                Cow::Owned(vec![]),
                None,
            )
            .unwrap();
        }
        ctx
    }

    fn parent(ctx: &TransactionContext<'_>) -> Option<Pubkey> {
        InstructionContextFrames::try_from(ctx)
            .unwrap()
            .find_program_id_of_parent_of_current_instruction()
            .copied()
    }

    /// Missing current instructions error, and top-level instructions have no caller.
    #[test]
    fn find_parent_program_empty_frames() {
        let mut ctx = context(&[Pubkey::new_unique()], 1);
        assert!(matches!(
            InstructionContextFrames::try_from(&ctx),
            Err(InstructionError::CallDepth)
        ));
        ctx.push().unwrap();
        assert_eq!(parent(&ctx), None);
    }

    /// Native caller links ignore preconfigured future instructions, including nested CPIs.
    #[test]
    fn find_parent_of_nested_instruction() {
        let programs: Vec<_> = (0..4).map(|_| Pubkey::new_unique()).collect();
        let mut ctx = context(&programs, 3);
        ctx.push().unwrap();
        ctx.configure_next_cpi_for_tests(3, vec![], vec![]).unwrap();
        ctx.push().unwrap();
        assert_eq!(parent(&ctx), Some(programs[0]));
        ctx.configure_next_cpi_for_tests(1, vec![], vec![]).unwrap();
        ctx.push().unwrap();
        assert_eq!(parent(&ctx), Some(programs[3]));
    }

    /// Completed CPI branches never change the caller of a later top-level instruction.
    #[test]
    fn find_parent_in_large_deeply_nexted_instructions() {
        let programs: Vec<_> = (0..9).map(|_| Pubkey::new_unique()).collect();
        let mut ctx = context(&programs, 3);
        for top in 0..3 {
            ctx.push().unwrap();
            assert_eq!(parent(&ctx), None);
            let child = 3 + top * 2;
            ctx.configure_next_cpi_for_tests(child as u16, vec![], vec![])
                .unwrap();
            ctx.push().unwrap();
            assert_eq!(parent(&ctx), Some(programs[top]));
            ctx.configure_next_cpi_for_tests(
                (child + 1) as u16,
                vec![],
                vec![],
            )
            .unwrap();
            ctx.push().unwrap();
            assert_eq!(parent(&ctx), Some(programs[child]));
            ctx.pop().unwrap();
            ctx.pop().unwrap();
            ctx.pop().unwrap();
        }
    }
}
