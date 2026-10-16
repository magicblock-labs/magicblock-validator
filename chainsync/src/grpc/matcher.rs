use ahash::AHashMap;
use delegation_program_api::{
    pda::delegation_record_pda_from_delegated_account, state::DelegationRecord,
};
use solana_account::AccountBuilder;
use solana_pubkey::Pubkey;
use yellowstone_grpc_proto::prelude::SubscribeUpdateAccountInfo;

use crate::delegation::{self, Delegation};

/// Matches application accounts with their delegation record PDA updates in one slot.
pub(super) struct Matcher {
    /// Delegations for this validator and authority-free confinement may activate.
    authority: Pubkey,
    /// Slot shared by pending observations; replay may move backward.
    slot: u64,
    /// Unmatched account/record updates and rejection markers, keyed by delegation-record address.
    pending: AHashMap<Pubkey, PendingDelegation>,
}

/// DLP-owned application account state awaiting its matching delegation-record update.
struct PendingAccount {
    /// Application account, not the record PDA.
    pubkey: Pubkey,
    /// Raw streamed account state awaiting restoration of its original owner.
    update: SubscribeUpdateAccountInfo,
}

impl PendingAccount {
    /// Builds Engine account state from the application update and its matching record.
    fn resolve(self, record: PendingRecord) -> Delegation {
        let account =
            AccountBuilder::default().lamports(self.update.lamports).data(self.update.data);
        Delegation {
            pubkey: self.pubkey,
            account: delegation::account(account, &record.metadata),
            record: record.data,
            source_program: record.metadata.owner,
        }
    }
}

/// Validated delegation record update awaiting its application account.
struct PendingRecord {
    /// Validated ownership, authority, and delegation slot.
    metadata: DelegationRecord,
    /// Full record, including appended actions.
    data: Vec<u8>,
}

/// One side of an unresolved same-slot delegation.
enum PendingDelegation {
    /// Application account update arrived first.
    Account(PendingAccount),
    /// Delegation record arrived first.
    Record(PendingRecord),
    /// Record must not activate an application account update in this slot.
    Ignored,
}

impl Matcher {
    /// Starts an empty same-slot matcher for this authority and authority-free confinement.
    pub(super) fn new(authority: Pubkey) -> Self {
        Self {
            authority,
            slot: 0,
            pending: AHashMap::new(),
        }
    }

    /// Discards incomplete matches whenever the stream changes slots.
    pub(super) fn set_slot(&mut self, slot: u64) {
        if self.slot != slot {
            self.pending.clear();
            self.slot = slot;
        }
    }

    /// Matches records for this authority or confinement only when their delegation slot matches.
    /// Other records block matching updates for the rest of the observed slot.
    pub(super) fn record(
        &mut self,
        key: Pubkey,
        account: &SubscribeUpdateAccountInfo,
        metadata: &DelegationRecord,
    ) -> Option<Delegation> {
        // Keep an ignored marker so a later application update cannot form a match.
        if !delegation::belongs_to(metadata, self.authority)
            || metadata.delegation_slot != self.slot
        {
            return self.observe(key, PendingDelegation::Ignored);
        }
        let record = PendingRecord {
            metadata: *metadata,
            data: account.data.clone(),
        };
        self.observe(key, PendingDelegation::Record(record))
    }

    /// Adds an application account update to the pending same-slot delegation match.
    pub(super) fn account(
        &mut self,
        pubkey: Pubkey,
        update: SubscribeUpdateAccountInfo,
    ) -> Option<Delegation> {
        // Record updates are keyed by this account's derived delegation-record PDA.
        let record = delegation_record_pda_from_delegated_account(&pubkey);
        let pending = PendingAccount { pubkey, update };
        self.observe(record, PendingDelegation::Account(pending))
    }

    /// Completes a delegation when both matching updates are present.
    fn observe(&mut self, key: Pubkey, incoming: PendingDelegation) -> Option<Delegation> {
        use PendingDelegation::*;

        let Some(current) = self.pending.remove(&key) else {
            self.pending.insert(key, incoming);
            return None;
        };
        let pending = match (current, incoming) {
            (Account(account), Record(record)) | (Record(record), Account(account)) => {
                return Some(account.resolve(record));
            }
            // A rejected record blocks matching for this slot. Otherwise retain
            // the latest account or record update until its counterpart arrives.
            (Ignored, _) | (_, Ignored) => Ignored,
            (_, pending) => pending,
        };
        self.pending.insert(key, pending);
        None
    }
}
