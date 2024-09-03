use std::{
    collections::HashSet,
    sync::{Arc, RwLock},
};

use solana_sdk::{account::Account, pubkey::Pubkey, signature::Signature};

use crate::{AccountDumper, AccountDumperResult};

#[derive(Debug, Clone, Default)]
pub struct AccountDumperStub {
    dumped_system_accounts: Arc<RwLock<HashSet<Pubkey>>>,
    dumped_pda_accounts: Arc<RwLock<HashSet<Pubkey>>>,
    dumped_delegated_accounts: Arc<RwLock<HashSet<Pubkey>>>,
    dumped_program_ids: Arc<RwLock<HashSet<Pubkey>>>,
}

impl AccountDumper for AccountDumperStub {
    fn dump_system_account(
        &self,
        pubkey: &Pubkey,
        _account: &Account,
        _lamports: Option<u64>,
    ) -> AccountDumperResult<Signature> {
        self.dumped_system_accounts.write().unwrap().insert(*pubkey);
        Ok(Signature::new_unique())
    }

    fn dump_pda_account(
        &self,
        pubkey: &Pubkey,
        _account: &Account,
    ) -> AccountDumperResult<Signature> {
        self.dumped_pda_accounts.write().unwrap().insert(*pubkey);
        Ok(Signature::new_unique())
    }

    fn dump_delegated_account(
        &self,
        pubkey: &Pubkey,
        _account: &Account,
        _owner: &Pubkey,
    ) -> AccountDumperResult<Signature> {
        self.dumped_delegated_accounts
            .write()
            .unwrap()
            .insert(*pubkey);
        Ok(Signature::new_unique())
    }

    fn dump_program_accounts(
        &self,
        program_id_pubkey: &Pubkey,
        _program_id_account: &Account,
        _program_data_pubkey: &Pubkey,
        _program_data_account: &Account,
        _program_idl_snapshot: Option<(Pubkey, Account)>,
    ) -> AccountDumperResult<Vec<Signature>> {
        self.dumped_program_ids
            .write()
            .unwrap()
            .insert(*program_id_pubkey);
        Ok(vec![Signature::new_unique()])
    }
}

impl AccountDumperStub {
    pub fn was_dumped_as_system_account(&self, pubkey: &Pubkey) -> bool {
        self.dumped_system_accounts.read().unwrap().contains(pubkey)
    }

    pub fn was_dumped_as_pda_account(&self, pubkey: &Pubkey) -> bool {
        self.dumped_pda_accounts.read().unwrap().contains(pubkey)
    }

    pub fn was_dumped_as_delegated_account(&self, pubkey: &Pubkey) -> bool {
        self.dumped_delegated_accounts
            .read()
            .unwrap()
            .contains(pubkey)
    }

    pub fn was_dumped_as_program_id(&self, pubkey: &Pubkey) -> bool {
        self.dumped_program_ids.read().unwrap().contains(pubkey)
    }

    pub fn was_untouched(&self, pubkey: &Pubkey) -> bool {
        !self.was_dumped_as_system_account(pubkey)
            && !self.was_dumped_as_pda_account(pubkey)
            && !self.was_dumped_as_delegated_account(pubkey)
            && !self.was_dumped_as_program_id(pubkey)
    }

    pub fn clear_history(&self) {
        self.dumped_system_accounts.write().unwrap().clear();
        self.dumped_pda_accounts.write().unwrap().clear();
        self.dumped_delegated_accounts.write().unwrap().clear();
        self.dumped_program_ids.write().unwrap().clear();
    }
}
