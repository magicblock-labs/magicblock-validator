use std::{
    collections::{HashMap, HashSet},
    sync::RwLock,
};

use async_trait::async_trait;
use conjunto_transwise::{
    errors::{TranswiseError, TranswiseResult},
    trans_account_meta::TransactionAccountsHolder,
    validated_accounts::{ValidateAccountsConfig, ValidatedAccounts},
    TransactionAccountsExtractor, ValidatedAccountsProvider,
};
use sleipnir_accounts::{
    errors::AccountsResult, AccountCloner, InternalAccountProvider,
};
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
    cloned_accounts: RwLock<HashSet<Pubkey>>,
}

impl AccountClonerStub {
    pub fn did_clone(&self, pubkey: &Pubkey) -> bool {
        self.cloned_accounts.read().unwrap().contains(pubkey)
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
    ) -> AccountsResult<Signature> {
        self.cloned_accounts.write().unwrap().insert(*pubkey);
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
                    LockboxError(_) => unimplemented!("Cannot clone"),
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
                }
            }
            None => Ok(ValidatedAccounts {
                readonly: transaction_accounts.readonly.clone(),
                writable: transaction_accounts.writable.clone(),
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
