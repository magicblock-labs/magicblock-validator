use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

use crate::{DelegateArgs, DelegationProgramDiscriminator};

/// Instruction builder for `Delegate`.
///
/// ### Accounts:
///
///   0. `[writable, signer]` payer
///   1. `[signer]` delegated_account
#[derive(Clone, Debug, Default)]
pub struct DelegateBuilder {
    pub payer: Pubkey,
    pub delegated_account: Pubkey,
    pub remaining_accounts: Vec<AccountMeta>,
    pub args: DelegateArgs,
}

impl DelegateBuilder {
    pub fn instruction(&self) -> Instruction {
        Instruction {
            program_id: crate::COMPRESSED_DELEGATION_ID,
            accounts: [
                &[
                    AccountMeta::new(self.payer, true),
                    AccountMeta::new_readonly(self.delegated_account, true),
                ],
                self.remaining_accounts.as_slice(),
            ]
            .concat(),
            data: [
                &(DelegationProgramDiscriminator::Delegate as u64)
                    .to_le_bytes(),
                borsh::to_vec(&self.args).unwrap().as_slice(),
            ]
            .concat(),
        }
    }
}
