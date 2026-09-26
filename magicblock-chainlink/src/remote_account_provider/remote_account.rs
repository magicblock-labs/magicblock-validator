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

#[derive(Debug, PartialEq, Eq)]
pub struct RemoteAccountState {
    /// The most recent remote image, not necessarily materialized locally.
    pub account: AccountSharedData,
    pub source: RemoteAccountUpdateSource,
}

impl Clone for RemoteAccountState {
    fn clone(&self) -> Self {
        Self {
            account: AccountSharedData::from(self.account.owned()),
            source: self.source.clone(),
        }
    }
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
            account: account.build(),
            source,
        })
    }
    pub fn slot(&self) -> u64 {
        match self {
            RemoteAccount::Found(state) => state.account.slot(),
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

    pub fn fresh_account(&self) -> Option<&AccountSharedData> {
        match self {
            RemoteAccount::Found(state) => Some(&state.account),
            _ => None,
        }
    }

    pub fn into_fresh_account(self) -> Option<AccountSharedData> {
        match self {
            RemoteAccount::Found(state) => Some(state.account),
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
