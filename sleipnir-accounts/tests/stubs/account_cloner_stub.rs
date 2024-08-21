use std::{collections::HashMap, sync::RwLock};

use async_trait::async_trait;
use sleipnir_accounts::{errors::AccountsResult, AccountCloner};
use sleipnir_program::sleipnir_instruction::AccountModification;
use solana_sdk::{account::Account, pubkey::Pubkey, signature::Signature};

#[derive(Default, Debug)]
pub struct AccountClonerStub {
    cloned_accounts: RwLock<HashMap<Pubkey, Option<AccountModification>>>,
}

#[allow(unused)] // used in tests
impl AccountClonerStub {
    pub fn did_clone(&self, pubkey: &Pubkey) -> bool {
        self.cloned_accounts.read().unwrap().contains_key(pubkey)
    }

    pub fn did_override_owner(&self, pubkey: &Pubkey, owner: &Pubkey) -> bool {
        let read_lock = self.cloned_accounts.read().unwrap();
        if let Some(overrides) = read_lock.get(pubkey) {
            overrides.as_ref().and_then(|x| x.owner.as_ref()) == Some(owner)
        } else {
            false
        }
    }

    pub fn did_override_lamports(
        &self,
        pubkey: &Pubkey,
        lamports: u64,
    ) -> bool {
        let read_lock = self.cloned_accounts.read().unwrap();
        if let Some(overrides) = read_lock.get(pubkey) {
            let override_lamports =
                overrides.as_ref().and_then(|x| x.lamports.as_ref());
            override_lamports == Some(&lamports)
        } else {
            false
        }
    }

    pub fn did_not_override_owner(&self, pubkey: &Pubkey) -> bool {
        let read_lock = self.cloned_accounts.read().unwrap();
        if let Some(overrides) = read_lock.get(pubkey) {
            overrides.as_ref().and_then(|x| x.owner.as_ref()).is_none()
        } else {
            true
        }
    }

    pub fn did_not_override_lamports(&self, pubkey: &Pubkey) -> bool {
        let read_lock = self.cloned_accounts.read().unwrap();
        if let Some(overrides) = read_lock.get(pubkey) {
            overrides
                .as_ref()
                .and_then(|x| x.lamports.as_ref())
                .is_none()
        } else {
            true
        }
    }

    pub fn clear(&self) {
        self.cloned_accounts.write().unwrap().clear();
    }
}

#[async_trait]
impl AccountCloner for AccountClonerStub {
    async fn clone_account(
        &self,
        pubkey: &Pubkey,
        _account: Option<Account>,
        overrides: Option<AccountModification>,
    ) -> AccountsResult<Vec<Signature>> {
        self.cloned_accounts
            .write()
            .unwrap()
            .insert(*pubkey, overrides);
        Ok(vec![Signature::new_unique()])
    }
}
