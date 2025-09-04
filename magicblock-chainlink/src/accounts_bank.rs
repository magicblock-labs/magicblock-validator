use solana_account::AccountSharedData;
use solana_pubkey::Pubkey;

// -----------------
// Trait
// -----------------
pub trait AccountsBank: Send + Sync + 'static {
    fn get_account(&self, pubkey: &Pubkey) -> Option<AccountSharedData>;
    fn remove_account(&self, pubkey: &Pubkey);
}

#[cfg(any(test, feature = "dev-context"))]
pub mod mock {
    use log::*;
    use solana_account::WritableAccount;
    use std::{collections::HashMap, fmt, sync::Mutex};

    use crate::blacklisted_accounts;

    use super::*;
    #[derive(Default)]
    pub struct AccountsBankStub {
        pub accounts: Mutex<HashMap<Pubkey, AccountSharedData>>,
    }

    impl AccountsBankStub {
        pub fn insert(&self, pubkey: Pubkey, account: AccountSharedData) {
            trace!("Inserting account: {pubkey}");
            self.accounts.lock().unwrap().insert(pubkey, account);
        }

        pub fn get(&self, pubkey: &Pubkey) -> Option<AccountSharedData> {
            self.accounts.lock().unwrap().get(pubkey).cloned()
        }

        pub fn set_owner(&self, pubkey: &Pubkey, owner: Pubkey) -> &Self {
            trace!("Setting owner for account: {pubkey} to {owner}");
            let mut accounts = self.accounts.lock().unwrap();
            if let Some(account) = accounts.get_mut(pubkey) {
                account.set_owner(owner);
            } else {
                panic!("Account not found in bank: {pubkey}");
            }
            self
        }

        fn set_delegated(&self, pubkey: &Pubkey, delegated: bool) -> &Self {
            trace!("Setting delegated for account: {pubkey} to {delegated}");
            let mut accounts = self.accounts.lock().unwrap();
            if let Some(account) = accounts.get_mut(pubkey) {
                account.set_delegated(delegated);
            } else {
                panic!("Account not found in bank: {pubkey}");
            }
            self
        }

        pub fn delegate(&self, pubkey: &Pubkey) -> &Self {
            self.set_delegated(pubkey, true)
        }

        pub fn undelegate(&self, pubkey: &Pubkey) -> &Self {
            self.set_delegated(pubkey, false)
        }

        /// Here we mark the account as undelegated in our validator via:
        /// - set_owner to delegation program
        /// - set_delegated to false
        pub fn force_undelegation(&self, pubkey: &Pubkey) {
            // NOTE: that the validator will also have to set flip the delegated flag like
            //       we do here.
            //       See programs/magicblock/src/schedule_transactions/process_schedule_commit.rs :172
            self.set_owner(pubkey, ephemeral_rollups_sdk::id())
                .undelegate(pubkey);
        }

        #[allow(dead_code)]
        pub fn dump_account_keys(&self, include_blacklisted: bool) -> String {
            let mut output = String::new();
            output.push_str("AccountsBank {\n");
            let blacklisted_accounts =
                blacklisted_accounts(&Pubkey::default(), &Pubkey::default());
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

    impl AccountsBank for AccountsBankStub {
        fn get_account(&self, pubkey: &Pubkey) -> Option<AccountSharedData> {
            self.accounts.lock().unwrap().get(pubkey).cloned()
        }
        fn remove_account(&self, pubkey: &Pubkey) {
            self.accounts.lock().unwrap().remove(pubkey);
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
