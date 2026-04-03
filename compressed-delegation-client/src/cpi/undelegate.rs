use solana_account_info::AccountInfo;
use solana_cpi::invoke_signed;
use solana_program_error::ProgramResult;

use crate::cpi::helpers::{collect_account_infos, remaining_to_metas};
use crate::UndelegateArgs;

/// CPI helper for `Undelegate` (on-chain callers).
///
/// ### Accounts:
///
///   0. `[writable, signer]` payer
///   1. `[writable]` delegated_account
///   2. `[]` owner_program
///   3. `[]` system_program
#[derive(Clone, Debug)]
pub struct UndelegateCpi<'a> {
    pub payer: AccountInfo<'a>,
    pub delegated_account: AccountInfo<'a>,
    pub owner_program: AccountInfo<'a>,
    pub system_program: AccountInfo<'a>,
    pub remaining_accounts: Vec<(AccountInfo<'a>, bool, bool)>,
    pub args: UndelegateArgs,
}

impl<'a> UndelegateCpi<'a> {
    pub fn invoke(&self) -> ProgramResult {
        self.invoke_signed(&[])
    }

    pub fn invoke_signed(&self, signers: &[&[&[u8]]]) -> ProgramResult {
        let ix = crate::builders::UndelegateBuilder {
            payer: *self.payer.key,
            delegated_account: *self.delegated_account.key,
            owner_program: *self.owner_program.key,
            system_program: *self.system_program.key,
            remaining_accounts: remaining_to_metas(&self.remaining_accounts),
            args: self.args.clone(),
        }
        .instruction();
        let infos = collect_account_infos(
            &[
                self.payer.clone(),
                self.delegated_account.clone(),
                self.owner_program.clone(),
                self.system_program.clone(),
            ],
            &self.remaining_accounts,
        );
        invoke_signed(&ix, &infos, signers)
    }
}
