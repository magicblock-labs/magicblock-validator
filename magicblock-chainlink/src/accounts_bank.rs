#[cfg(any(test, feature = "dev-context"))]
pub mod mock {
    use log::*;
    use magicblock_core::traits::AccountsBank;
    use scc::HashMap;
    use solana_account::{AccountSharedData, WritableAccount};
    use solana_pubkey::Pubkey;
    use std::fmt;

    use crate::blacklisted_accounts;

    #[derive(Default)]
    pub struct AccountsBankStub {
        pub accounts: HashMap<Pubkey, AccountSharedData>,
    }

    impl AccountsBankStub {
        pub fn insert(&self, pubkey: Pubkey, account: AccountSharedData) {
            trace!("Inserting account: {pubkey}");
            self.accounts.upsert(pubkey, account);
        }

        pub fn get(&self, pubkey: &Pubkey) -> Option<AccountSharedData> {
            self.accounts.get(pubkey).map(|acc| acc.get().clone())
        }

        pub fn set_owner(&self, pubkey: &Pubkey, owner: Pubkey) -> &Self {
            trace!("Setting owner for account: {pubkey} to {owner}");
            if let Some(mut account) = self.accounts.get(pubkey) {
                account.set_owner(owner);
            } else {
                panic!("Account not found in bank: {pubkey}");
            }
            self
        }

        fn set_delegated(&self, pubkey: &Pubkey, delegated: bool) -> &Self {
            trace!("Setting delegated for account: {pubkey} to {delegated}");
            if let Some(mut account) = self.accounts.get(pubkey) {
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
            self.set_owner(pubkey, dlp::id()).undelegate(pubkey);
        }

        #[allow(dead_code)]
        pub fn dump_account_keys(&self, include_blacklisted: bool) -> String {
            let mut output = String::new();
            output.push_str("AccountsBank {\n");
            let blacklisted_accounts =
                blacklisted_accounts(&Pubkey::default(), &Pubkey::default());
            self.accounts.scan(|pk, _| {
                if !include_blacklisted && blacklisted_accounts.contains(pk) {
                    return;
                }
                output.push_str(&format!("{pk},\n"));
            });
            output.push_str("} ");
            output.push_str(&format!("{} total", self.accounts.len()));
            output
        }
    }

    impl AccountsBank for AccountsBankStub {
        fn get_account(&self, pubkey: &Pubkey) -> Option<AccountSharedData> {
            self.get(pubkey)
        }
        fn remove_account(&self, pubkey: &Pubkey) {
            self.accounts.remove(pubkey);
        }
    }

    impl fmt::Display for AccountsBankStub {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "AccountsBankStub {{")?;
            self.accounts.scan(|pubkey, acc| {
                let _ = write!(f, "\n  - {pubkey}{acc:?}");
            });
            write!(f, "}}\nTotal {} accounts", self.accounts.len())
        }
    }
}
