use std::collections::{HashMap, HashSet};

use async_trait::async_trait;
use conjunto_transwise::{
    errors::{TranswiseError, TranswiseResult},
    transaction_accounts_holder::TransactionAccountsHolder,
    validated_accounts::{
        LockConfig, ValidateAccountsConfig, ValidatedAccounts,
        ValidatedReadonlyAccount, ValidatedWritableAccount,
    },
    CommitFrequency, ValidatedAccountsProvider,
};
use solana_sdk::{account::Account, pubkey::Pubkey};

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
                        account: Some(Account::default()),
                    })
                    .collect(),
                writable: transaction_accounts
                    .writable
                    .iter()
                    .map(|x| ValidatedWritableAccount {
                        pubkey: *x,
                        account: match self.new_accounts.contains(x) {
                            true => None,
                            false => Some(Account {
                                owner: match self.with_owners.get(x) {
                                    Some(owner) => *owner,
                                    None => Pubkey::new_unique(),
                                },
                                ..Account::default()
                            }),
                        },
                        lock_config: self.with_owners.get(x).as_ref().map(
                            |owner| LockConfig {
                                owner: **owner,
                                commit_frequency: CommitFrequency::default(),
                            },
                        ),
                        is_payer: self.payers.contains(x),
                    })
                    .collect(),
            }),
        }
    }
}
