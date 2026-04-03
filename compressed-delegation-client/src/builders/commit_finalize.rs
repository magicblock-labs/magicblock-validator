use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

use crate::{CommitAndFinalizeArgs, DelegationProgramDiscriminator};

/// Instruction builder for `Commit`.
///
/// ### Accounts:
///
///   0. `[writable, signer]` validator
///   1. `[]` delegated_account
#[derive(Clone, Debug, Default)]
pub struct CommitAndFinalizeBuilder {
    pub validator: Pubkey,
    pub delegated_account: Pubkey,
    pub remaining_accounts: Vec<AccountMeta>,
    pub args: CommitAndFinalizeArgs,
}

impl CommitAndFinalizeBuilder {
    pub fn instruction(&self) -> Instruction {
        Instruction {
            program_id: crate::COMPRESSED_DELEGATION_ID,
            accounts: [
                &[
                    AccountMeta::new(self.validator, true),
                    AccountMeta::new_readonly(self.delegated_account, false),
                ],
                self.remaining_accounts.as_slice(),
            ]
            .concat(),
            data: [
                &(DelegationProgramDiscriminator::CommitAndFinalize as u64)
                    .to_le_bytes(),
                borsh::to_vec(&self.args).unwrap().as_slice(),
            ]
            .concat(),
        }
    }
}
