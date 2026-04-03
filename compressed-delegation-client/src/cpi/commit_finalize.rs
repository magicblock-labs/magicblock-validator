use solana_account_info::AccountInfo;
use solana_cpi::invoke_signed;
use solana_program_error::ProgramResult;

use crate::cpi::helpers::{collect_account_infos, remaining_to_metas};
use crate::CommitAndFinalizeArgs;

/// CPI helper for `CommitAndFinalize` (on-chain callers).
///
/// ### Accounts:
///
///   0. `[writable, signer]` validator
///   1. `[]` delegated_account
#[derive(Clone, Debug)]
pub struct CommitAndFinalizeCpi<'a> {
    pub validator: AccountInfo<'a>,
    pub delegated_account: AccountInfo<'a>,
    pub remaining_accounts: Vec<(AccountInfo<'a>, bool, bool)>,
    pub args: CommitAndFinalizeArgs,
}

impl<'a> CommitAndFinalizeCpi<'a> {
    pub fn invoke(&self) -> ProgramResult {
        self.invoke_signed(&[])
    }

    pub fn invoke_signed(&self, signers: &[&[&[u8]]]) -> ProgramResult {
        let ix = crate::builders::CommitAndFinalizeBuilder {
            validator: *self.validator.key,
            delegated_account: *self.delegated_account.key,
            remaining_accounts: remaining_to_metas(&self.remaining_accounts),
            args: self.args.clone(),
        }
        .instruction();
        let infos = collect_account_infos(
            &[self.validator.clone(), self.delegated_account.clone()],
            &self.remaining_accounts,
        );
        invoke_signed(&ix, &infos, signers)
    }
}
