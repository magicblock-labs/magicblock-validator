use std::{
    collections::HashSet,
    sync::{Arc, RwLock},
};

use conjunto_transwise::AccountChainSnapshotShared;
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
        _program_idl_snapshot: Option<AccountChainSnapshotShared>,
    ) -> AccountDumperResult<Vec<Signature>> {
        self.dumped_program_ids
            .write()
            .unwrap()
            .insert(*program_id_pubkey);
        Ok(vec![Signature::new_unique()])
    }
}

impl AccountDumperStub {
    pub fn get_dumped_system_accounts(&self) -> HashSet<Pubkey> {
        self.dumped_system_accounts.read().unwrap().clone()
    }

    pub fn get_dumped_pda_accounts(&self) -> HashSet<Pubkey> {
        self.dumped_pda_accounts.read().unwrap().clone()
    }

    pub fn get_dumped_delegated_accounts(&self) -> HashSet<Pubkey> {
        self.dumped_delegated_accounts.read().unwrap().clone()
    }

    pub fn get_dumped_program_ids(&self) -> HashSet<Pubkey> {
        self.dumped_program_ids.read().unwrap().clone()
    }
}
