use std::{collections::HashMap, str::FromStr, sync::RwLock};

use async_trait::async_trait;
use sleipnir_accounts::{errors::AccountsResult, AccountCloner};
use sleipnir_mutator::AccountModification;
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

    #[allow(dead_code)] // will use in test assertions
    pub fn did_override_owner(&self, pubkey: &Pubkey, owner: &Pubkey) -> bool {
        let read_lock = self.cloned_accounts.read().unwrap();
        let overrides = read_lock.get(pubkey);
        if overrides.is_none() {
            eprintln!("ERR: No overrides for pubkey: {}", pubkey);
            return false;
        }
        let overrides = overrides.unwrap();
        overrides
            .as_ref()
            .and_then(|x| x.owner.as_ref())
            .map(|o| Pubkey::from_str(o).unwrap())
            == Some(*owner)
    }

    pub fn did_override_lamports(
        &self,
        pubkey: &Pubkey,
        lamports: u64,
    ) -> bool {
        let read_lock = self.cloned_accounts.read().unwrap();
        let overrides = read_lock.get(pubkey);
        if overrides.is_none() {
            return false;
        }
        let overrides = overrides.unwrap();
        let override_lamports =
            overrides.as_ref().and_then(|x| x.lamports.as_ref());
        override_lamports == Some(&lamports)
    }

    pub fn did_not_override_owner(&self, pubkey: &Pubkey) -> bool {
        let read_lock = self.cloned_accounts.read().unwrap();
        let overrides = read_lock.get(pubkey).unwrap();
        overrides.as_ref().and_then(|x| x.owner.as_ref()).is_none()
    }

    pub fn did_not_override_lamports(&self, pubkey: &Pubkey) -> bool {
        let read_lock = self.cloned_accounts.read().unwrap();
        let overrides = read_lock.get(pubkey).unwrap();
        overrides
            .as_ref()
            .and_then(|x| x.lamports.as_ref())
            .is_none()
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
    ) -> AccountsResult<Signature> {
        self.cloned_accounts
            .write()
            .unwrap()
            .insert(*pubkey, overrides);
        Ok(Signature::new_unique())
    }
}
