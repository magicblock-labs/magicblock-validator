//! Account roles, transport destinations, and mirror state.

use engine::AccountAccessor;
use solana_account::{AccountBuilder, AccountMode};
use solana_loader_v3_interface::get_program_data_address;
use solana_pubkey::Pubkey;

/// Account role that determines companion fetches and live updates during synchronization.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccountProperty {
    /// Fee payer; checked for delegation and subscribed unless delegated to this Engine.
    Payer,
    /// Checks delegation without subscribing; otherwise refetches as Readonly or Program.
    Writable,
    /// Read-only transaction account; subscribed unless resolved as delegated to this Engine.
    Readonly,
    /// Executable program; tracks Loader V3 upgrades through ProgramData,
    /// and other supported loaders through the program account itself.
    Program,
}

/// An unleased request for [`super::ChainSync::ensure`] to acquire only if missing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChainSyncAccount {
    /// Address under which the account is made available in Engine.
    pub pubkey: Pubkey,
    /// Role in the transaction that needs this account.
    pub property: AccountProperty,
}

/// An exclusively leased account selected for [`super::ChainSync::sync`].
///
/// The lease must belong to the synchronizer's Engine. Selecting resident state
/// requires lifecycle evidence permitting replacement; synchronization does not
/// choose replacement eligibility on the caller's behalf.
pub struct SyncTarget<'engine> {
    /// Mutation ownership retained through primary and companion fetches.
    pub(crate) accessor: AccountAccessor<'engine>,
    /// Companion and normalization requirements for the current fetch.
    pub(crate) property: AccountProperty,
    /// Primary tracking carried through companion fetches; HTTP fetches wait for acknowledgement.
    pub(crate) subscribed: bool,
}

impl<'engine> SyncTarget<'engine> {
    /// Selects a leased target; synchronization acquires its required subscriptions.
    pub fn new(accessor: AccountAccessor<'engine>, property: AccountProperty) -> Self {
        Self {
            accessor,
            property,
            subscribed: false,
        }
    }
}

/// Remote address watched by a transport and optional Loader V3 program to update.
///
/// Ordinary subscriptions watch and update the same address. Loader V3 subscriptions
/// watch ProgramData but materialize its normalized ELF at the local program address.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct AccountSubscription {
    pub(super) pubkey: Pubkey,
    /// Program address to materialize ProgramData ELF under; `None` updates `pubkey` itself.
    pub(super) program: Option<Pubkey>,
}

impl AccountSubscription {
    pub(super) fn local_pubkey(self) -> Pubkey {
        self.program.unwrap_or(self.pubkey)
    }

    /// Watches the derived ProgramData address while materializing updates under the program address.
    pub(super) fn program_data(pubkey: Pubkey) -> Self {
        Self {
            pubkey: get_program_data_address(&pubkey),
            program: Some(pubkey),
        }
    }

    /// Returns the subscriptions that can update this local address:
    /// the address itself and its derived ProgramData address.
    pub(super) fn for_account(pubkey: Pubkey) -> [Self; 2] {
        [Self { pubkey, program: None }, Self::program_data(pubkey)]
    }
}

/// Assigns the absence/read-only mode without interpreting the account's data.
pub(super) fn mirror(account: AccountBuilder) -> AccountBuilder {
    let mode = if account.read().lamports() > 0 {
        AccountMode::ReadOnly
    } else {
        AccountMode::Uninit
    };
    account.mode(mode)
}
