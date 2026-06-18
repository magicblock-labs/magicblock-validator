use solana_instruction::{error::InstructionError, Instruction};
use solana_program_runtime::invoke_context::InvokeContext;
use solana_pubkey::Pubkey;

pub(crate) trait NativeInvoke {
    fn native_invoke(
        &mut self,
        instruction: Instruction,
        signers: &[Pubkey],
    ) -> Result<(), InstructionError>;
}

impl NativeInvoke for InvokeContext<'_, '_> {
    fn native_invoke(
        &mut self,
        instruction: Instruction,
        signers: &[Pubkey],
    ) -> Result<(), InstructionError> {
        self.prepare_next_cpi_instruction(instruction, signers)?;
        let mut compute_units_consumed = 0;
        self.process_instruction(
            &mut compute_units_consumed,
            &mut Default::default(),
        )?;
        Ok(())
    }
}
