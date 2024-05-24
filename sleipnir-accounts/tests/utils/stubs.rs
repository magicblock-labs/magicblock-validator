use std::{collections::HashMap, str::FromStr, sync::RwLock};

use async_trait::async_trait;
use conjunto_transwise::{
    errors::{TranswiseError, TranswiseResult},
    trans_account_meta::TransactionAccountsHolder,
    validated_accounts::{
        ValidateAccountsConfig, ValidatedAccounts, ValidatedReadonlyAccount,
        ValidatedWritableAccount,
    },
    TransactionAccountsExtractor, ValidatedAccountsProvider,
};
use sleipnir_accounts::{
    errors::AccountsResult, AccountCloner, InternalAccountProvider,
};
use sleipnir_mutator::AccountModification;
use solana_sdk::{
    account::AccountSharedData,
    pubkey::Pubkey,
    signature::Signature,
    transaction::{SanitizedTransaction, VersionedTransaction},
};

// -----------------
// InternalAccountProviderStub
// -----------------
#[derive(Default, Debug)]
pub struct InternalAccountProviderStub {
    accounts: HashMap<Pubkey, AccountSharedData>,
}
impl InternalAccountProviderStub {
    pub fn add(&mut self, pubkey: Pubkey, account: AccountSharedData) {
        self.accounts.insert(pubkey, account);
    }
}

impl InternalAccountProvider for InternalAccountProviderStub {
    fn get_account(&self, pubkey: &Pubkey) -> Option<AccountSharedData> {
        self.accounts.get(pubkey).cloned()
    }
}

// -----------------
// AccountClonerStub
// -----------------
#[derive(Default, Debug)]
pub struct AccountClonerStub {
    cloned_accounts: RwLock<HashMap<Pubkey, Option<Pubkey>>>,
}

impl AccountClonerStub {
    pub fn did_clone(&self, pubkey: &Pubkey) -> bool {
        self.cloned_accounts.read().unwrap().contains_key(pubkey)
    }

    #[allow(dead_code)] // will use in test assertions
    pub fn did_override_owner(&self, pubkey: &Pubkey, owner: &Pubkey) -> bool {
        let read_lock = self.cloned_accounts.read().unwrap();
        let stored_owner = read_lock.get(pubkey).unwrap();
        stored_owner.as_ref() == Some(owner)
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
        overrides: Option<AccountModification>,
    ) -> AccountsResult<Signature> {
        self.cloned_accounts.write().unwrap().insert(
            *pubkey,
            overrides
                .as_ref()
                .and_then(|x| x.owner.as_ref())
                .map(|o| Pubkey::from_str(o.as_str()).unwrap()),
        );
        Ok(Signature::new_unique())
    }
}

// -----------------
// ValidatedAccountsProviderStub
// -----------------
#[derive(Debug)]
pub struct ValidatedAccountsProviderStub {
    validation_error: Option<TranswiseError>,
}

impl ValidatedAccountsProviderStub {
    pub fn valid() -> Self {
        Self {
            validation_error: None,
        }
    }

    pub fn invalid(error: TranswiseError) -> Self {
        Self {
            validation_error: Some(error),
        }
    }
}

#[async_trait]
impl ValidatedAccountsProvider for ValidatedAccountsProviderStub {
    async fn validated_accounts_from_versioned_transaction(
        &self,
        _tx: &VersionedTransaction,
        _config: &ValidateAccountsConfig,
    ) -> TranswiseResult<ValidatedAccounts> {
        unimplemented!()
    }

    async fn validated_accounts_from_sanitized_transaction(
        &self,
        _tx: &SanitizedTransaction,
        _config: &ValidateAccountsConfig,
    ) -> TranswiseResult<ValidatedAccounts> {
        unimplemented!()
    }

    async fn validate_accounts(
        &self,
        transaction_accounts: &TransactionAccountsHolder,
        _config: &ValidateAccountsConfig,
    ) -> TranswiseResult<ValidatedAccounts> {
        match &self.validation_error {
            Some(error) => {
                use TranswiseError::*;
                match error {
                    NotAllWritablesLocked { locked, unlocked } => {
                        Err(TranswiseError::NotAllWritablesLocked {
                            locked: locked.clone(),
                            unlocked: unlocked.clone(),
                        })
                    },
                    WritablesIncludeInconsistentAccounts { inconsistent } => {
                        Err(TranswiseError::WritablesIncludeInconsistentAccounts {
                            inconsistent: inconsistent.clone(),
                        })
                    }
                    WritablesIncludeNewAccounts { new_accounts } => {
                        Err(TranswiseError::WritablesIncludeNewAccounts {
                            new_accounts: new_accounts.clone(),
                        })
                    },
                    _ => {
                        unimplemented!()
                    }
                }
            }
            None => Ok(ValidatedAccounts {
                readonly: transaction_accounts
                    .readonly
                    .iter()
                    .map(|x| ValidatedReadonlyAccount {
                        pubkey: *x,
                        is_program: Some(false),
                    })
                    .collect(),
                writable: transaction_accounts
                    .writable
                    .iter()
                    .map(|x| ValidatedWritableAccount {
                        pubkey: *x,
                        owner: None,
                    })
                    .collect(),
            }),
        }
    }
}

impl TransactionAccountsExtractor for ValidatedAccountsProviderStub {
    fn accounts_from_versioned_transaction(
        &self,
        _tx: &VersionedTransaction,
    ) -> TransactionAccountsHolder {
        unimplemented!("We don't exxtract during tests")
    }

    fn accounts_from_sanitized_transaction(
        &self,
        _tx: &SanitizedTransaction,
    ) -> TransactionAccountsHolder {
        unimplemented!("We don't exxtract during tests")
    }
}
