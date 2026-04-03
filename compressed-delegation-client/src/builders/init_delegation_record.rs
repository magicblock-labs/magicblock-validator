use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

use crate::{DelegationProgramDiscriminator, InitDelegationRecordArgs};

/// Instruction builder for `InitDelegationRecord`.
///
/// ### Accounts:
///
///   0. `[writable, signer]` payer
///   1. `[signer]` delegated_account
#[derive(Clone, Debug)]
pub struct InitDelegationRecordBuilder {
    pub payer: Pubkey,
    pub delegated_account: Pubkey,
    pub remaining_accounts: Vec<AccountMeta>,
    pub args: InitDelegationRecordArgs,
}

impl InitDelegationRecordBuilder {
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
                &(DelegationProgramDiscriminator::InitDelegationRecord as u64)
                    .to_le_bytes(),
                borsh::to_vec(&self.args).unwrap().as_slice(),
            ]
            .concat(),
        }
    }
}
