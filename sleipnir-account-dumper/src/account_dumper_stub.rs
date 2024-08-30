use std::sync::{Arc, RwLock};

use solana_sdk::{account::Account, pubkey::Pubkey, signature::Signature};

use crate::{AccountDumper, AccountDumperResult};

#[derive(Debug, Clone, Default)]
pub struct AccountDumperStub {
    dumped_system_accounts: Arc<RwLock<Vec<Pubkey>>>,
    dumped_pda_accounts: Arc<RwLock<Vec<Pubkey>>>,
    dumped_delegated_accounts: Arc<RwLock<Vec<Pubkey>>>,
    dumped_programs: Arc<RwLock<Vec<Pubkey>>>,
}

impl AccountDumperStub {
    fn append<T>(container: &RwLock<Vec<T>>, value: T)
    where
        T: Clone,
    {
        container
            .write()
            .expect("RwLock of AccountDumperStub poisoned")
            .push(value)
    }
    fn list<T>(container: &RwLock<Vec<T>>) -> Vec<T>
    where
        T: Clone,
    {
        container
            .read()
            .expect("RwLock of AccountDumperStub poisoned")
            .clone()
    }
}

impl AccountDumper for AccountDumperStub {
    fn dump_system_account(
        &self,
        pubkey: &Pubkey,
        _account: &Account,
        _lamports: Option<u64>,
    ) -> AccountDumperResult<Signature> {
        AccountDumperStub::append(&self.dumped_system_accounts, *pubkey);
        Ok(Signature::new_unique())
    }
    fn dump_pda_account(
        &self,
        pubkey: &Pubkey,
        _account: &Account,
    ) -> AccountDumperResult<Signature> {
        AccountDumperStub::append(&self.dumped_pda_accounts, *pubkey);
        Ok(Signature::new_unique())
    }
    fn dump_delegated_account(
        &self,
        pubkey: &Pubkey,
        _account: &Account,
        _owner: &Pubkey,
    ) -> AccountDumperResult<Signature> {
        AccountDumperStub::append(&self.dumped_delegated_accounts, *pubkey);
        Ok(Signature::new_unique())
    }
    fn dump_program(
        &self,
        program_id_pubkey: &Pubkey,
        _program_id_account: &Account,
        _program_data_pubkey: &Pubkey,
        _program_data_account: &Account,
        _program_idl: Option<(&Pubkey, &Account)>,
    ) -> AccountDumperResult<Vec<Signature>> {
        AccountDumperStub::append(&self.dumped_programs, *program_id_pubkey);
        Ok(vec![Signature::new_unique()])
    }
}

impl AccountDumperStub {
    pub fn list_dumped_system_account(&self) -> Vec<Pubkey> {
        AccountDumperStub::list(&self.dumped_system_accounts)
    }
    pub fn list_dumped_pda_account(&self) -> Vec<Pubkey> {
        AccountDumperStub::list(&self.dumped_pda_accounts)
    }
    pub fn list_dumped_delegated_account(&self) -> Vec<Pubkey> {
        AccountDumperStub::list(&self.dumped_delegated_accounts)
    }
    pub fn list_dumped_programs(&self) -> Vec<Pubkey> {
        AccountDumperStub::list(&self.dumped_programs)
    }
}
