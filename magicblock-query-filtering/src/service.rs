use std::{collections::HashSet, sync::Arc};

use magicblock_accounts_db::traits::AccountsBank;
use magicblock_aml::RiskService;
use magicblock_config::config::QueryFilteringConfig;
use solana_account::ReadableAccount;
use solana_pubkey::Pubkey;
use thiserror::Error;

use crate::{
    auth::{AuthError, AuthService},
    types::{
        ChallengeResponse, LoginRequest, LoginResponse, Permission,
        PermissionEntry, PermissionError,
    },
};

pub type QueryFilteringResult<T> = Result<T, QueryFilteringError>;

#[derive(Debug, Error)]
pub enum QueryFilteringError {
    #[error(transparent)]
    Permission(#[from] PermissionError),
    #[error(transparent)]
    Auth(#[from] AuthError),
    #[error("access denied: transaction touches a restricted account")]
    AccessDenied,
}

pub struct QueryFilteringService {
    auth_service: Arc<AuthService>,
}

impl QueryFilteringService {
    pub fn try_new(
        config: &QueryFilteringConfig,
        risk: Option<Arc<RiskService>>,
    ) -> QueryFilteringResult<Self> {
        Ok(Self {
            auth_service: Arc::new(AuthService::try_new(
                &config.jwt_secret,
                config.token_expiry_days,
                config.challenge_ttl_seconds,
                risk,
            )?),
        })
    }

    pub fn verify_token(&self, token: &str) -> QueryFilteringResult<String> {
        Ok(self.auth_service.verify_token(token)?)
    }

    pub fn get_challenge(&self, user_pubkey: &str) -> ChallengeResponse {
        ChallengeResponse {
            challenge: self.auth_service.generate_challenge(user_pubkey),
        }
    }

    pub async fn login(
        &self,
        request: LoginRequest,
    ) -> QueryFilteringResult<LoginResponse> {
        Ok(self.auth_service.login(request).await?)
    }

    /// Graceful-shutdown hook invoked during validator teardown. The service
    /// owns no background tasks today, so there is nothing to release.
    pub fn stop(&self) {}
}

pub fn filter_account<T>(
    value: Option<T>,
    permission: &PermissionEntry,
    user: &Pubkey,
) -> Option<T> {
    permission
        .access_for(user)
        .account
        .then_some(value)
        .flatten()
}

pub fn filter_accounts<T>(
    values: Vec<Option<T>>,
    permissions: &[PermissionEntry],
    user: &Pubkey,
) -> Vec<Option<T>> {
    values
        .into_iter()
        .zip(permissions)
        .map(|(value, permission)| filter_account(value, permission, user))
        .collect()
}

pub fn filter_signatures<T>(
    values: Vec<T>,
    permission: &PermissionEntry,
    user: &Pubkey,
) -> Vec<T> {
    if permission.access_for(user).signatures {
        values
    } else {
        Vec::new()
    }
}

pub fn filter_program_accounts<T>(
    values: Vec<(Pubkey, T)>,
    program_permission: &PermissionEntry,
    user: &Pubkey,
) -> Vec<(Pubkey, T)> {
    if program_permission.access_for(user).account {
        values
    } else {
        Vec::new()
    }
}

pub fn filter_keyed_accounts<T>(
    values: Vec<(Pubkey, T)>,
    permissions: &[PermissionEntry],
    user: &Pubkey,
) -> Vec<(Pubkey, T)> {
    values
        .into_iter()
        .zip(permissions)
        .filter_map(|(value, permission)| {
            permission.access_for(user).account.then_some(value)
        })
        .collect()
}

/// Loads the on-chain permission for a single account, returning
/// [`PermissionEntry::Unrestricted`] when no permission account exists.
///
/// Two cases short-circuit to [`PermissionEntry::Unrestricted`]:
/// - The queried account is itself a permission account (owned by the
///   permission program). Permission accounts hold the ACL rules themselves
///   and must remain visible to everyone — they are never subject to
///   permission filtering.
/// - The permission PDA isn't owned by the permission program
pub fn permission_for_account(
    accounts_db: &impl AccountsBank,
    account: &Pubkey,
) -> QueryFilteringResult<PermissionEntry> {
    if let Some(account_data) = accounts_db.get_account(account) {
        if account_data.owner() == &crate::PERMISSION_PROGRAM_ID {
            return Ok(PermissionEntry::Unrestricted);
        }
    }
    let permission_account = Permission::pda(account);
    let Some(account) = accounts_db.get_account(&permission_account) else {
        return Ok(PermissionEntry::Unrestricted);
    };
    if account.owner() != &crate::PERMISSION_PROGRAM_ID {
        return Ok(PermissionEntry::Unrestricted);
    }
    Ok(PermissionEntry::from_permission(Permission::decode(
        account.data(),
    )?))
}

/// Loads permissions for a batch of accounts, preserving input order so the
/// result can be zipped positionally with the corresponding accounts.
pub fn permissions_for_accounts(
    accounts_db: &impl AccountsBank,
    accounts: &[Pubkey],
) -> QueryFilteringResult<Vec<PermissionEntry>> {
    accounts
        .iter()
        .map(|account| permission_for_account(accounts_db, account))
        .collect()
}

pub fn visible_transaction_accounts(
    accounts_db: &impl AccountsBank,
    accounts: &[Pubkey],
    user: &Pubkey,
) -> QueryFilteringResult<HashSet<Pubkey>> {
    let permissions = permissions_for_accounts(accounts_db, accounts)?;
    Ok(accounts
        .iter()
        .zip(permissions)
        .filter_map(|(account, permission)| {
            permission.access_for(user).account.then_some(*account)
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use magicblock_accounts_db::AccountsDbResult;
    use solana_account::AccountSharedData;

    use super::*;
    use crate::{
        types::{Member, RestrictedEntry},
        PERMISSION_PROGRAM_ID,
    };

    #[derive(Default)]
    struct MockAccountsBank {
        accounts: HashMap<Pubkey, AccountSharedData>,
    }

    impl MockAccountsBank {
        fn insert_account_with_permission(
            &mut self,
            pubkey: Pubkey,
            account: AccountSharedData,
            permission: Permission,
            ephemeral: bool,
        ) {
            self.accounts.insert(pubkey, account);

            let permission_data = permission.encode(ephemeral);
            let mut permission_account = AccountSharedData::new(
                1000000,
                permission_data.len(),
                &PERMISSION_PROGRAM_ID,
            );
            permission_account.set_data_from_slice(&permission_data);
            self.accounts
                .insert(Permission::pda(&pubkey), permission_account);
        }
    }

    impl AccountsBank for MockAccountsBank {
        fn get_account(&self, pubkey: &Pubkey) -> Option<AccountSharedData> {
            self.accounts.get(pubkey).cloned()
        }

        fn remove_account(&self, _pubkey: &Pubkey) {}

        fn remove_where(
            &self,
            _predicate: impl FnMut(&Pubkey, &AccountSharedData) -> bool,
        ) -> AccountsDbResult<usize> {
            Ok(0)
        }
    }

    #[test]
    fn account_filter_preserves_shape() {
        let allowed = Pubkey::new_unique();
        let denied = Pubkey::new_unique();
        let permission = PermissionEntry::Restricted(RestrictedEntry {
            members: vec![Member {
                pubkey: allowed,
                flags: u8::MAX,
            }],
            are_program_restricted: true,
        });

        assert_eq!(filter_account(Some(42), &permission, &allowed), Some(42));
        assert_eq!(filter_account(Some(42), &permission, &denied), None);
    }

    #[test]
    fn visible_transaction_accounts_returns_only_visible_accounts_for_restricted_permission(
    ) {
        let mut accounts_db = MockAccountsBank::default();
        let accounts = vec![Pubkey::new_unique(), Pubkey::new_unique()];
        let user = Pubkey::new_unique();
        let denied_user = Pubkey::new_unique();
        accounts_db.insert_account_with_permission(
            accounts[0],
            AccountSharedData::new(1000000, 0, &Pubkey::default()),
            Permission::new(
                accounts[0],
                Some(vec![Member {
                    pubkey: user,
                    flags: u8::MAX,
                }]),
            ),
            false,
        );

        let visible_accounts =
            visible_transaction_accounts(&accounts_db, &accounts, &user)
                .unwrap();
        assert_eq!(visible_accounts, HashSet::from([accounts[0], accounts[1]]));

        let visible_accounts =
            visible_transaction_accounts(&accounts_db, &accounts, &denied_user)
                .unwrap();
        assert_eq!(visible_accounts, HashSet::from([accounts[1]]));
    }

    #[test]
    fn permission_for_account_is_unrestricted_when_no_permission_exists() {
        let accounts_db = MockAccountsBank::default();
        let account = Pubkey::new_unique();

        let permission =
            permission_for_account(&accounts_db, &account).unwrap();

        assert_eq!(permission, PermissionEntry::Unrestricted);
    }

    #[test]
    fn permission_for_account_decodes_restricted_permission() {
        let mut accounts_db = MockAccountsBank::default();
        let account = Pubkey::new_unique();
        let member = Pubkey::new_unique();
        accounts_db.insert_account_with_permission(
            account,
            AccountSharedData::new(1000000, 0, &Pubkey::default()),
            Permission::new(
                account,
                Some(vec![Member {
                    pubkey: member,
                    flags: u8::MAX,
                }]),
            ),
            false,
        );

        let permission =
            permission_for_account(&accounts_db, &account).unwrap();

        assert!(permission.access_for(&member).account);
        assert!(!permission.access_for(&Pubkey::new_unique()).account);
    }

    #[test]
    fn permission_for_account_treats_permission_accounts_as_public() {
        // A permission account at `Permission::pda(counter)` is itself owned
        // by the permission program. When someone queries that permission
        // account directly, the filter layer must report it as public — the
        // ACL data is never subject to ACL filtering.
        let mut accounts_db = MockAccountsBank::default();
        let counter = Pubkey::new_unique();
        let member = Pubkey::new_unique();
        accounts_db.insert_account_with_permission(
            counter,
            AccountSharedData::new(1_000_000, 0, &Pubkey::default()),
            Permission::new(
                counter,
                Some(vec![Member {
                    pubkey: member,
                    flags: u8::MAX,
                }]),
            ),
            false,
        );

        let permission_pda = Permission::pda(&counter);

        // The data account (counter) is private to `member`.
        let counter_permission =
            permission_for_account(&accounts_db, &counter).unwrap();
        assert!(!counter_permission.access_for(&Pubkey::new_unique()).account);

        // The permission account itself is public — every caller sees it.
        let perm_for_perm =
            permission_for_account(&accounts_db, &permission_pda).unwrap();
        assert_eq!(perm_for_perm, PermissionEntry::Unrestricted);
        assert!(perm_for_perm.access_for(&Pubkey::new_unique()).account);
    }

    #[test]
    fn permission_for_account_reports_decode_context() {
        let mut accounts_db = MockAccountsBank::default();
        let account = Pubkey::new_unique();
        let permission_account = Permission::pda(&account);
        let corrupt_data = vec![7, 8, 9];
        let mut permission = AccountSharedData::new(
            1000000,
            corrupt_data.len(),
            &PERMISSION_PROGRAM_ID,
        );
        permission.set_data_from_slice(&corrupt_data);
        accounts_db.accounts.insert(permission_account, permission);

        let err = permission_for_account(&accounts_db, &account).unwrap_err();

        match err {
            QueryFilteringError::Permission(
                PermissionError::DataTooShort { len, min_len },
            ) => {
                assert_eq!(len, 3);
                assert_eq!(min_len, 34);
            }
            err => panic!("unexpected error: {err:?}"),
        }
    }

    #[test]
    fn visible_transaction_accounts_returns_only_visible_accounts_for_unrestricted_permission(
    ) {
        let accounts_db = MockAccountsBank::default();
        let accounts = vec![Pubkey::new_unique(), Pubkey::new_unique()];
        let user = Pubkey::new_unique();
        let visible_accounts =
            visible_transaction_accounts(&accounts_db, &accounts, &user)
                .unwrap();
        assert_eq!(visible_accounts, HashSet::from([accounts[0], accounts[1]]));
    }
}
