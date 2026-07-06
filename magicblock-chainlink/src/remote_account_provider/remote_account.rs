use engine::Engine;
use solana_account::{
    Account, AccountFieldPatch, AccountMode, AccountSharedData,
    ReadableAccount, WritableAccount,
};
use solana_clock::Slot;
use solana_pubkey::Pubkey;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteAccountUpdateSource {
    Fetch,
    Subscription,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedAccount {
    /// The most recent remote state of the account that is not stored in the bank yet.
    /// The account maybe in our bank at this point, but with a stale remote state.
    /// The only accounts that are always more fresh than the remote version are accounts
    /// delegated to us.
    /// Therefore we never fetch them again or subscribe to them once we cloned them into
    /// our bank once.
    /// The committor service will let us know once they are being undelegated at which point
    /// we subscribe to them and fetch the latest state.
    Fresh(AccountSharedData),
    /// Most _fresh_ accounts are stored in the bank before the transaction needing
    /// them proceeds. Delegation records are not stored.
    Bank((Pubkey, Slot)),
}

impl ResolvedAccount {
    pub fn resolved_account_shared_data(
        &self,
        engine: &Engine,
    ) -> Option<ResolvedAccountSharedData> {
        match self {
            ResolvedAccount::Fresh(account) => {
                Some(ResolvedAccountSharedData::Fresh(account.clone()))
            }
            ResolvedAccount::Bank((pubkey, _)) => engine
                .accounts()
                .get(pubkey)
                .ok()
                .flatten()
                .map(ResolvedAccountSharedData::Bank),
        }
    }
}

/// Same as [ResolvedAccount], but with the account data fetched from the bank.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedAccountSharedData {
    Fresh(AccountSharedData),
    Bank(AccountSharedData),
}

impl ResolvedAccountSharedData {
    pub fn owner(&self) -> &Pubkey {
        use ResolvedAccountSharedData::*;
        match self {
            Fresh(account) => account.owner(),
            Bank(account) => account.owner(),
        }
    }

    pub fn set_owner(&mut self, owner: Pubkey) -> &mut Self {
        use ResolvedAccountSharedData::*;
        match self {
            Fresh(account) => account.set_owner(owner),
            Bank(account) => account.set_owner(owner),
        }
        self
    }

    pub fn data(&self) -> &[u8] {
        use ResolvedAccountSharedData::*;
        match self {
            Fresh(account) => account.data(),
            Bank(account) => account.data(),
        }
    }

    pub fn lamports(&self) -> u64 {
        use ResolvedAccountSharedData::*;
        match self {
            Fresh(account) => account.lamports(),
            Bank(account) => account.lamports(),
        }
    }

    pub fn executable(&self) -> bool {
        use ResolvedAccountSharedData::*;
        match self {
            Fresh(account) => account.executable(),
            Bank(account) => account.executable(),
        }
    }

    pub fn delegated(&self) -> bool {
        use ResolvedAccountSharedData::*;
        match self {
            Fresh(account) => account.is(AccountMode::Delegated),
            Bank(account) => account.is(AccountMode::Delegated),
        }
    }

    pub fn confined(&self) -> bool {
        use ResolvedAccountSharedData::*;
        match self {
            Fresh(account) => account.is(AccountMode::Ephemeral),
            Bank(account) => account.is(AccountMode::Ephemeral),
        }
    }

    /// Sets the account's mode.
    ///
    /// Delegation state is a single exclusive mode rather than the independent
    /// `delegated`/`confined` flags this replaced, so callers must resolve the
    /// final mode up front; setting one aspect at a time would silently discard
    /// the others.
    pub fn set_mode(&mut self, mode: AccountMode) -> &mut Self {
        use ResolvedAccountSharedData::*;
        match self {
            Fresh(account) => account.set_mode(mode),
            Bank(account) => account.set_mode(mode),
        }
        self
    }

    pub fn set_remote_slot(&mut self, remote_slot: Slot) -> &mut Self {
        use ResolvedAccountSharedData::*;
        let patch = AccountFieldPatch::Slot(remote_slot);
        match self {
            Fresh(account) => patch.apply(account),
            Bank(account) => patch.apply(account),
        }
        self
    }

    pub fn account_shared_data(&self) -> &AccountSharedData {
        use ResolvedAccountSharedData::*;
        match self {
            Fresh(account) => account,
            Bank(account) => account,
        }
    }

    pub fn account_shared_data_cloned(&self) -> AccountSharedData {
        use ResolvedAccountSharedData::*;
        match self {
            Fresh(account) => account.clone(),
            Bank(account) => account.clone(),
        }
    }

    pub fn into_account_shared_data(self) -> AccountSharedData {
        use ResolvedAccountSharedData::*;
        match self {
            Fresh(account) => account,
            Bank(account) => account,
        }
    }

    pub fn remote_slot(&self) -> Slot {
        use ResolvedAccountSharedData::*;
        match self {
            Fresh(account) => account.slot(),
            Bank(account) => account.slot(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteAccountState {
    pub account: ResolvedAccount,
    pub source: RemoteAccountUpdateSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteAccount {
    NotFound(Slot),
    Found(RemoteAccountState),
}

impl RemoteAccount {
    pub fn from_fresh_account(
        account: Account,
        slot: u64,
        source: RemoteAccountUpdateSource,
    ) -> Self {
        let mut account_shared_data = AccountSharedData::from(account);
        AccountFieldPatch::Slot(slot).apply(&mut account_shared_data);
        Self::from_fresh_account_shared_data(account_shared_data, source)
    }

    pub(crate) fn from_fresh_account_shared_data(
        account_shared_data: AccountSharedData,
        source: RemoteAccountUpdateSource,
    ) -> Self {
        RemoteAccount::Found(RemoteAccountState {
            account: ResolvedAccount::Fresh(account_shared_data),
            source,
        })
    }
    /// Returns the fresh remote account if it was just updated, otherwise tries the bank
    pub fn account(
        &self,
        engine: &Engine,
    ) -> Option<ResolvedAccountSharedData> {
        match self {
            // Fresh remote account, not in the bank yet
            RemoteAccount::Found(RemoteAccountState {
                account: ResolvedAccount::Fresh(remote_account),
                ..
            }) => {
                Some(ResolvedAccountSharedData::Fresh(remote_account.clone()))
            }
            // Most up to date version of account from the bank
            RemoteAccount::Found(RemoteAccountState {
                account: ResolvedAccount::Bank((pubkey, _)),
                ..
            }) => engine
                .accounts()
                .get(pubkey)
                .ok()
                .flatten()
                .map(ResolvedAccountSharedData::Bank),
            // Account not fetched/subbed nor in the bank
            RemoteAccount::NotFound(_) => None,
        }
    }
    pub fn slot(&self) -> u64 {
        match self {
            RemoteAccount::Found(RemoteAccountState { account, .. }) => {
                match account {
                    ResolvedAccount::Fresh(account_shared_data) => {
                        account_shared_data.slot()
                    }
                    ResolvedAccount::Bank((_, slot)) => *slot,
                }
            }
            RemoteAccount::NotFound(slot) => *slot,
        }
    }
    pub fn source(&self) -> Option<RemoteAccountUpdateSource> {
        match self {
            RemoteAccount::Found(RemoteAccountState { source, .. }) => {
                Some(source.clone())
            }
            RemoteAccount::NotFound(_) => None,
        }
    }

    pub fn is_found(&self) -> bool {
        !matches!(self, RemoteAccount::NotFound(_))
    }

    pub fn fresh_account(&self) -> Option<AccountSharedData> {
        match self {
            RemoteAccount::Found(RemoteAccountState {
                account: ResolvedAccount::Fresh(account),
                ..
            }) => Some(account.clone()),
            _ => None,
        }
    }

    pub fn fresh_lamports(&self) -> Option<u64> {
        self.fresh_account().map(|acc| acc.lamports())
    }

    pub fn owner(&self) -> Option<Pubkey> {
        self.fresh_account().map(|acc| *acc.owner())
    }

    pub fn is_owned_by_delegation_program(&self) -> bool {
        self.owner().is_some_and(|owner| owner.eq(&dlp_api::id()))
    }
}
