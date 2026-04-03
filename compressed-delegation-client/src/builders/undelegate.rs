use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

use crate::{DelegationProgramDiscriminator, UndelegateArgs};

/// Instruction builder for `Undelegate`.
///
/// ### Accounts:
///
///   0. `[writable, signer]` payer
///   1. `[writable]` delegated_account
///   2. `[]` owner_program
///   3. `[]` system_program
#[derive(Clone, Debug, Default)]
pub struct UndelegateBuilder {
    pub payer: Pubkey,
    pub delegated_account: Pubkey,
    pub owner_program: Pubkey,
    pub system_program: Pubkey,
    pub remaining_accounts: Vec<AccountMeta>,
    pub args: UndelegateArgs,
}

impl UndelegateBuilder {
    pub fn instruction(&self) -> Instruction {
        Instruction {
            program_id: crate::COMPRESSED_DELEGATION_ID,
            accounts: [
                &[
                    AccountMeta::new(self.payer, true),
                    AccountMeta::new(self.delegated_account, false),
                    AccountMeta::new_readonly(self.owner_program, false),
                    AccountMeta::new_readonly(self.system_program, false),
                ],
                self.remaining_accounts.as_slice(),
            ]
            .concat(),
            data: [
                &(DelegationProgramDiscriminator::Undelegate as u64)
                    .to_le_bytes(),
                borsh::to_vec(&self.args).unwrap().as_slice(),
            ]
            .concat(),
        }
    }
}
