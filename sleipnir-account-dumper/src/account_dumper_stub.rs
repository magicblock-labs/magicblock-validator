use std::{
    collections::HashSet,
    sync::{Arc, RwLock},
};

use solana_sdk::{account::Account, pubkey::Pubkey, signature::Signature};

use crate::{AccountDumper, AccountDumperResult};

#[derive(Debug, Clone, Default)]
pub struct AccountDumperStub {
    wallet_accounts: Arc<RwLock<HashSet<Pubkey>>>,
    undelegated_accounts: Arc<RwLock<HashSet<Pubkey>>>,
    delegated_accounts: Arc<RwLock<HashSet<Pubkey>>>,
    program_ids: Arc<RwLock<HashSet<Pubkey>>>,
    program_datas: Arc<RwLock<HashSet<Pubkey>>>,
    program_idls: Arc<RwLock<HashSet<Pubkey>>>,
}

impl AccountDumper for AccountDumperStub {
    fn dump_wallet_account(
        &self,
        pubkey: &Pubkey,
        _account: &Account,
        _lamports: Option<u64>,
    ) -> AccountDumperResult<Signature> {
        self.wallet_accounts.write().unwrap().insert(*pubkey);
        Ok(Signature::new_unique())
    }

    fn dump_undelegated_account(
        &self,
        pubkey: &Pubkey,
        _account: &Account,
    ) -> AccountDumperResult<Signature> {
        self.undelegated_accounts.write().unwrap().insert(*pubkey);
        Ok(Signature::new_unique())
    }

    fn dump_delegated_account(
        &self,
        pubkey: &Pubkey,
        _account: &Account,
        _owner: &Pubkey,
    ) -> AccountDumperResult<Signature> {
        self.delegated_accounts.write().unwrap().insert(*pubkey);
        Ok(Signature::new_unique())
    }

    fn dump_program_accounts(
        &self,
        program_id_pubkey: &Pubkey,
        _program_id_account: &Account,
        program_data_pubkey: &Pubkey,
        _program_data_account: &Account,
        program_idl: Option<(Pubkey, Account)>,
    ) -> AccountDumperResult<Signature> {
        self.program_ids.write().unwrap().insert(*program_id_pubkey);
        self.program_datas
            .write()
            .unwrap()
            .insert(*program_data_pubkey);
        if let Some(program_idl) = program_idl {
            self.program_idls.write().unwrap().insert(program_idl.0);
        }
        Ok(Signature::new_unique())
    }
}

impl AccountDumperStub {
    pub fn was_dumped_as_wallet_account(&self, pubkey: &Pubkey) -> bool {
        self.wallet_accounts.read().unwrap().contains(pubkey)
    }
    pub fn was_dumped_as_pda_account(&self, pubkey: &Pubkey) -> bool {
        self.undelegated_accounts.read().unwrap().contains(pubkey)
    }
    pub fn was_dumped_as_delegated_account(&self, pubkey: &Pubkey) -> bool {
        self.delegated_accounts.read().unwrap().contains(pubkey)
    }

    pub fn was_dumped_as_program_id(&self, pubkey: &Pubkey) -> bool {
        self.program_ids.read().unwrap().contains(pubkey)
    }
    pub fn was_dumped_as_program_data(&self, pubkey: &Pubkey) -> bool {
        self.program_datas.read().unwrap().contains(pubkey)
    }
    pub fn was_dumped_as_program_idl(&self, pubkey: &Pubkey) -> bool {
        self.program_idls.read().unwrap().contains(pubkey)
    }

    pub fn was_untouched(&self, pubkey: &Pubkey) -> bool {
        !self.was_dumped_as_wallet_account(pubkey)
            && !self.was_dumped_as_pda_account(pubkey)
            && !self.was_dumped_as_delegated_account(pubkey)
            && !self.was_dumped_as_program_id(pubkey)
            && !self.was_dumped_as_program_data(pubkey)
            && !self.was_dumped_as_program_idl(pubkey)
    }

    pub fn clear_history(&self) {
        self.wallet_accounts.write().unwrap().clear();
        self.undelegated_accounts.write().unwrap().clear();
        self.delegated_accounts.write().unwrap().clear();
        self.program_ids.write().unwrap().clear();
        self.program_datas.write().unwrap().clear();
        self.program_idls.write().unwrap().clear();
    }
}
