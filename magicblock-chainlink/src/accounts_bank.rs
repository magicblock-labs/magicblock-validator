use engine::Engine;
use solana_account::AccountSharedData;
use solana_pubkey::Pubkey;

/// Read access to the validator's account store.
///
/// Chainlink only ever reads through this trait; every mutation it performs
/// goes through the engine's MagicRoot program via [`crate::cloner::Cloner`],
/// so there is deliberately no write half here.
pub trait AccountsBank: Send + Sync + 'static {
    fn get_account(&self, pubkey: &Pubkey) -> Option<AccountSharedData>;
}

impl AccountsBank for Engine {
    fn get_account(&self, pubkey: &Pubkey) -> Option<AccountSharedData> {
        self.accounts().get(pubkey).ok().flatten()
    }
}

#[cfg(any(test, feature = "dev-context"))]
pub mod mock {
    use std::{collections::HashMap, fmt, sync::Mutex};

    use solana_account::{AccountMode, AccountSharedData, WritableAccount};
    use solana_pubkey::Pubkey;
    use tracing::*;

    use super::AccountsBank;
    use crate::blacklisted_accounts;

    #[derive(Default)]
    pub struct AccountsBankStub {
        pub accounts: Mutex<HashMap<Pubkey, AccountSharedData>>,
    }

    impl AccountsBankStub {
        pub fn insert(&self, pubkey: Pubkey, account: AccountSharedData) {
            trace!(pubkey = %pubkey, "Inserting account");
            self.accounts.lock().unwrap().insert(pubkey, account);
        }

        pub fn get(&self, pubkey: &Pubkey) -> Option<AccountSharedData> {
            self.accounts.lock().unwrap().get(pubkey).cloned()
        }

        pub fn set_owner(&self, pubkey: &Pubkey, owner: Pubkey) -> &Self {
            trace!(pubkey = %pubkey, owner = %owner, "Setting owner for account");
            let mut accounts = self.accounts.lock().unwrap();
            if let Some(account) = accounts.get_mut(pubkey) {
                account.set_owner(owner);
            } else {
                panic!("Account not found in bank: {pubkey}");
            }
            self
        }

        pub fn set_mode(&self, pubkey: &Pubkey, mode: AccountMode) -> &Self {
            trace!(pubkey = %pubkey, ?mode, "Setting mode for account");
            let mut accounts = self.accounts.lock().unwrap();
            if let Some(account) = accounts.get_mut(pubkey) {
                account.set_mode(mode);
            } else {
                panic!("Account not found in bank: {pubkey}");
            }
            self
        }

        pub fn delegate(&self, pubkey: &Pubkey) -> &Self {
            self.set_mode(pubkey, AccountMode::Delegated)
        }

        pub fn undelegate(&self, pubkey: &Pubkey) -> &Self {
            self.set_mode(pubkey, AccountMode::Transient)
        }

        /// Here we mark the account as undelegated in our validator via:
        /// - set_owner to delegation program
        /// - move it to the `Transient` mode
        pub fn force_undelegation(&self, pubkey: &Pubkey) {
            // NOTE: that the validator will also have to move the account out of
            //       the delegated mode like we do here.
            //       See programs/magicblock/src/schedule_transactions/process_schedule_commit.rs :172
            self.set_owner(pubkey, dlp_api::id()).undelegate(pubkey);
        }

        /// `Transient` is the engine's representation of a delegated account
        /// that is on its way back to readonly, i.e. the old `undelegating` flag.
        pub fn set_undelegating(
            &self,
            pubkey: &Pubkey,
            undelegating: bool,
        ) -> &Self {
            let mode = if undelegating {
                AccountMode::Transient
            } else {
                AccountMode::Delegated
            };
            self.set_mode(pubkey, mode)
        }

        pub fn dump_account_keys(&self, include_blacklisted: bool) -> String {
            let mut output = String::new();
            output.push_str("AccountsBank {\n");
            let blacklisted_accounts = blacklisted_accounts(&Pubkey::default());
            for pubkey in self.accounts.lock().unwrap().keys() {
                if !include_blacklisted && blacklisted_accounts.contains(pubkey)
                {
                    continue;
                }
                output.push_str(&format!("{pubkey},\n"));
            }
            output.push_str("} ");
            output.push_str(&format!(
                "{} total",
                self.accounts.lock().unwrap().len()
            ));
            output
        }
    }

    impl AccountsBankStub {
        /// Eviction is not part of [`AccountsBank`] — production code evicts
        /// through the engine, so this exists only for the test cloner stub.
        pub fn remove_account(&self, pubkey: &Pubkey) {
            self.accounts.lock().unwrap().remove(pubkey);
        }

        pub fn remove_where(
            &self,
            mut predicate: impl FnMut(&Pubkey, &AccountSharedData) -> bool,
        ) -> usize {
            let mut accounts = self.accounts.lock().unwrap();
            let initial_len = accounts.len();
            accounts.retain(|k, v| !predicate(k, v));
            initial_len - accounts.len()
        }
    }

    impl AccountsBank for AccountsBankStub {
        fn get_account(&self, pubkey: &Pubkey) -> Option<AccountSharedData> {
            self.accounts.lock().unwrap().get(pubkey).cloned()
        }
    }

    impl fmt::Display for AccountsBankStub {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "AccountsBankStub {{")?;
            for (pubkey, acc) in self.accounts.lock().unwrap().iter() {
                write!(f, "\n  - {pubkey}{acc:?}")?;
            }
            write!(
                f,
                "}}\nTotal {} accounts",
                self.accounts.lock().unwrap().len()
            )
        }
    }
}
