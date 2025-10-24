use std::sync::atomic::{AtomicU64, Ordering};

use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};
use solana_sdk_ids::system_program;

use crate::{
    common::{derive_anchor_idl_pda, derive_counter_pda, derive_shank_idl_pda},
    instruction::MiniInstruction,
};

pub struct MiniSdk {
    program_id: Pubkey,
}

impl MiniSdk {
    pub fn new(program_id: Pubkey) -> Self {
        Self { program_id }
    }

    pub fn counter_pda(&self, payer: &Pubkey) -> (Pubkey, u8) {
        derive_counter_pda(&self.program_id, payer)
    }

    pub fn shank_idl_pda(&self) -> (Pubkey, u8) {
        derive_shank_idl_pda(&self.program_id)
    }

    pub fn anchor_idl_pda(&self) -> (Pubkey, u8) {
        derive_anchor_idl_pda(&self.program_id)
    }

    pub fn init_instruction(&self, payer: &Pubkey) -> Instruction {
        let (counter_pubkey, _) = self.counter_pda(payer);

        Instruction::new_with_bytes(
            self.program_id,
            &Vec::from(MiniInstruction::Init),
            vec![
                AccountMeta::new(*payer, true),
                AccountMeta::new(counter_pubkey, false),
                AccountMeta::new_readonly(
                    Pubkey::new_from_array(system_program::id().to_bytes()),
                    false,
                ),
            ],
        )
    }

    pub fn increment_instruction(&self, payer: &Pubkey) -> Instruction {
        static INSTRUCTION_BUMP: AtomicU64 = AtomicU64::new(0);

        let (counter_pubkey, _) = self.counter_pda(payer);

        // Create unique instruction data with atomic bump
        let bump = INSTRUCTION_BUMP.fetch_add(1, Ordering::SeqCst);
        let mut instruction_data = Vec::from(MiniInstruction::Increment);
        instruction_data.extend_from_slice(&bump.to_le_bytes());

        Instruction::new_with_bytes(
            self.program_id,
            &instruction_data,
            vec![
                AccountMeta::new(*payer, true),
                AccountMeta::new(counter_pubkey, false),
            ],
        )
    }

    pub fn add_shank_idl_instruction(
        &self,
        payer: &Pubkey,
        idl: &[u8],
    ) -> Instruction {
        let (shank_idl_pubkey, _) = self.shank_idl_pda();

        Instruction::new_with_bytes(
            self.program_id,
            &Vec::from(MiniInstruction::AddShankIdl(idl.to_vec())),
            vec![
                AccountMeta::new(*payer, true),
                AccountMeta::new(shank_idl_pubkey, false),
                AccountMeta::new_readonly(
                    Pubkey::new_from_array(system_program::id().to_bytes()),
                    false,
                ),
            ],
        )
    }

    pub fn add_anchor_idl_instruction(
        &self,
        payer: &Pubkey,
        idl: &[u8],
    ) -> Instruction {
        let (anchor_idl_pubkey, _) = self.anchor_idl_pda();

        Instruction::new_with_bytes(
            self.program_id,
            &Vec::from(MiniInstruction::AddAnchorIdl(idl.to_vec())),
            vec![
                AccountMeta::new(*payer, true),
                AccountMeta::new(anchor_idl_pubkey, false),
                AccountMeta::new_readonly(
                    Pubkey::new_from_array(system_program::id().to_bytes()),
                    false,
                ),
            ],
        )
    }

    pub fn log_msg_instruction(
        &self,
        payer: &Pubkey,
        msg: &str,
    ) -> Instruction {
        Instruction::new_with_bytes(
            self.program_id,
            &Vec::from(MiniInstruction::LogMsg(msg.to_string())),
            vec![AccountMeta::new(*payer, true)],
        )
    }

    pub fn program_id(&self) -> Pubkey {
        self.program_id
    }
}
