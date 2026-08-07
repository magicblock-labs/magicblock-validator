use engine::Engine;
use solana_account::{
    Account, AccountBuilder, AccountSharedData, ReadableAccount,
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
        let account = AccountBuilder::from(account).slot(slot);
        Self::from_fresh_account_builder(account, source)
    }

    pub(crate) fn from_fresh_account_builder(
        account: AccountBuilder,
        source: RemoteAccountUpdateSource,
    ) -> Self {
        RemoteAccount::Found(RemoteAccountState {
            account: ResolvedAccount::Fresh(account.build()),
            source,
        })
    }
    /// Returns the fresh remote account if it was just updated, otherwise tries the bank
    pub fn account(&self, engine: &Engine) -> Option<AccountBuilder> {
        match self {
            // Fresh remote account, not in the bank yet
            RemoteAccount::Found(RemoteAccountState {
                account: ResolvedAccount::Fresh(remote_account),
                ..
            }) => Some(AccountBuilder::from(remote_account.clone())),
            // Most up to date version of account from the bank
            RemoteAccount::Found(RemoteAccountState {
                account: ResolvedAccount::Bank((pubkey, _)),
                ..
            }) => engine
                .accounts()
                .loader()
                .read(pubkey, |account| AccountBuilder::from(account.clone()))
                .ok()
                .flatten(),
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
