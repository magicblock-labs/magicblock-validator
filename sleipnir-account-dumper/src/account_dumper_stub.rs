use std::sync::RwLock;

use solana_sdk::{account::Account, pubkey::Pubkey, signature::Signature};

use crate::{AccountDumper, AccountDumperResult};

pub struct AccountDumperStub {}

impl AccountDumper for AccountDumperStub {
    fn dump_system_account(
        &self,
        pubkey: &Pubkey,
        account: &Account,
        lamports: Option<u64>,
    ) -> AccountDumperResult<Signature> {
        Ok(Signature::new_unique())
    }

    fn dump_pda_account(
        &self,
        pubkey: &Pubkey,
        account: &Account,
    ) -> AccountDumperResult<Signature> {
        Ok(Signature::new_unique())
    }

    fn dump_delegated_account(
        &self,
        pubkey: &Pubkey,
        account: &Account,
        owner: &Pubkey,
    ) -> AccountDumperResult<Signature> {
        Ok(Signature::new_unique())
    }

    fn dump_program(
        &self,
        program_id_pubkey: &Pubkey,
        program_id_account: &Account,
        program_data_pubkey: &Pubkey,
        program_data_account: &Account,
        program_idl: Option<(&Pubkey, &Account)>,
    ) -> AccountDumperResult<Vec<Signature>> {
        Ok(vec![Signature::new_unique()])
    }
}
