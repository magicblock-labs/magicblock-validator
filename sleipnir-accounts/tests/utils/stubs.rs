use std::{
    collections::{HashMap, HashSet},
    str::FromStr,
    sync::{Arc, RwLock},
};

use async_trait::async_trait;
//use conjunto_transwise::AccountChainState;
use conjunto_transwise::{
    errors::{TranswiseError, TranswiseResult},
    transaction_accounts_holder::TransactionAccountsHolder,
    validated_accounts::{
        ValidateAccountsConfig, ValidatedAccounts, ValidatedReadonlyAccount,
        ValidatedWritableAccount,
    },
    TransactionAccountsExtractor, ValidatedAccountsProvider,
};
use sleipnir_accounts::{
    errors::AccountsResult, AccountCloner, AccountCommitter,
    InternalAccountProvider,
};
use sleipnir_mutator::AccountModification;
use solana_sdk::account::Account;
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
    fn has_account(&self, pubkey: &Pubkey) -> bool {
        self.accounts.contains_key(pubkey)
    }
    fn get_account(&self, pubkey: &Pubkey) -> Option<AccountSharedData> {
        self.accounts.get(pubkey).cloned()
    }
}

// -----------------
// AccountClonerStub
// -----------------
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
        _account: Option<Arc<Account>>,
        overrides: Option<AccountModification>,
    ) -> AccountsResult<Signature> {
        self.cloned_accounts
            .write()
            .unwrap()
            .insert(*pubkey, overrides);
        Ok(Signature::new_unique())
    }
}

// -----------------
// AccountCommitter
// -----------------
#[derive(Debug, Default, Clone)]
pub struct AccountCommitterStub {
    committed_accounts: Arc<RwLock<HashMap<Pubkey, AccountSharedData>>>,
}

#[allow(unused)] // used in tests
impl AccountCommitterStub {
    pub fn len(&self) -> usize {
        self.committed_accounts.read().unwrap().len()
    }
    pub fn committed(&self, pubkey: &Pubkey) -> Option<AccountSharedData> {
        self.committed_accounts.read().unwrap().get(pubkey).cloned()
    }
}

#[async_trait]
impl AccountCommitter for AccountCommitterStub {
    async fn commit_account(
        &self,
        delegated_account: Pubkey,
        committed_state_data: AccountSharedData,
    ) -> AccountsResult<Option<Signature>> {
        self.committed_accounts
            .write()
            .unwrap()
            .insert(delegated_account, committed_state_data);
        Ok(Some(Signature::new_unique()))
    }
}

// -----------------
// ValidatedAccountsProviderStub
// -----------------
#[derive(Debug, Default)]
pub struct ValidatedAccountsProviderStub {
    validation_error: Option<TranswiseError>,
    payers: HashSet<Pubkey>,
    new_accounts: HashSet<Pubkey>,
    with_owners: HashMap<Pubkey, Pubkey>,
}

#[allow(unused)] // used in tests
impl ValidatedAccountsProviderStub {
    pub fn valid_default() -> Self {
        Self {
            validation_error: None,
            ..Default::default()
        }
    }
    pub fn valid(
        payers: HashSet<Pubkey>,
        new_accounts: HashSet<Pubkey>,
        with_owners: HashMap<Pubkey, Pubkey>,
    ) -> Self {
        Self {
            validation_error: None,
            payers,
            new_accounts,
            with_owners,
        }
    }

    pub fn invalid(error: TranswiseError) -> Self {
        Self {
            validation_error: Some(error),
            ..Default::default()
        }
    }
}

#[async_trait]
impl ValidatedAccountsProvider for ValidatedAccountsProviderStub {
    async fn validate_accounts(
        &self,
        transaction_accounts: &TransactionAccountsHolder,
        _config: &ValidateAccountsConfig,
    ) -> TranswiseResult<ValidatedAccounts> {
        match &self.validation_error {
            Some(error) => {
                use TranswiseError::*;
                match error {
                    NotAllWritablesDelegated {
                        writable_delegated_pubkeys,
                        writable_undelegated_non_payer_pubkeys,
                    } => Err(TranswiseError::NotAllWritablesDelegated {
                        writable_delegated_pubkeys: writable_delegated_pubkeys
                            .clone(),
                        writable_undelegated_non_payer_pubkeys:
                            writable_undelegated_non_payer_pubkeys.clone(),
                    }),
                    WritablesIncludeInconsistentAccounts {
                        writable_inconsistent_pubkeys,
                    } => Err(
                        TranswiseError::WritablesIncludeInconsistentAccounts {
                            writable_inconsistent_pubkeys:
                                writable_inconsistent_pubkeys.clone(),
                        },
                    ),
                    WritablesIncludeNewAccounts {
                        writable_new_pubkeys,
                    } => Err(TranswiseError::WritablesIncludeNewAccounts {
                        writable_new_pubkeys: writable_new_pubkeys.clone(),
                    }),
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
                        chain_state: todo!(),
                    })
                    .collect(),
                writable: transaction_accounts
                    .writable
                    .iter()
                    .map(|x| ValidatedWritableAccount {
                        pubkey: *x,
                        chain_state: todo!(),
                        /*
                        chain_state: AccountChainState::NewAccount,
                        lock_config: self.with_owners.get(x).as_ref().map(
                            |owner| LockConfig {
                                owner: **owner,
                                commit_frequency: CommitFrequency::default(),
                            },
                        ),
                        is_new: self.new_accounts.contains(x),
                         */
                        is_payer: self.payers.contains(x),
                    })
                    .collect(),
            }),
        }
    }
}

impl TransactionAccountsExtractor for ValidatedAccountsProviderStub {
    fn try_accounts_from_versioned_transaction(
        &self,
        _tx: &VersionedTransaction,
    ) -> TranswiseResult<TransactionAccountsHolder> {
        unimplemented!("We don't extract during tests")
    }

    fn try_accounts_from_sanitized_transaction(
        &self,
        _tx: &SanitizedTransaction,
    ) -> TranswiseResult<TransactionAccountsHolder> {
        unimplemented!("We don't extract during tests")
    }
}
