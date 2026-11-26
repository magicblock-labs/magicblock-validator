//! Ordered primary/companion fetch layouts for targets already selected under leases.

use std::iter::Peekable;

use delegation_program_api::pda::delegation_record_pda_from_delegated_account;
use solana_pubkey::Pubkey;

use crate::{
    AccountProperty::{Payer, Program, Readonly, Writable},
    AccountSubscription, SyncTarget,
    http::BATCH_LIMIT,
    websocket::Retention,
};

/// One selected target and its position in an ordered HTTP batch.
pub(super) struct PendingAccount<'engine> {
    /// Owns the lease and primary subscription across companion fetches.
    pub(super) target: SyncTarget<'engine>,
    /// Primary position in `FetchPlan::keys`; any companion immediately follows it.
    pub(super) index: usize,
    /// Meaning and response position of the companion selected by this fetch's role.
    pub(super) companion: Option<Companion>,
}

/// Companion positions share the primary's HTTP response and context slot.
#[derive(Clone, Copy)]
pub(super) enum Companion {
    /// Delegation metadata for a payer or writable account.
    Record(usize),
    /// Loader V3 backing image, fetched speculatively for every program request.
    ProgramData(usize),
}

/// One RPC-sized batch, including new subscriptions to acknowledge before fetching.
pub(super) struct FetchPlan<'engine> {
    /// Selected targets in the same order as their primary response positions.
    pub(super) accounts: Vec<PendingAccount<'engine>>,
    /// Primary and companion remote addresses in HTTP response order.
    pub(super) keys: Vec<Pubkey>,
    /// Subscription requests and whether to reuse a resident mirror's existing reference.
    pub(super) subscriptions: Vec<(AccountSubscription, Retention)>,
}

impl<'engine> FetchPlan<'engine> {
    /// Takes the next targets without exceeding the 100-entry RPC limit with companions.
    pub(super) fn next(targets: &mut Peekable<impl Iterator<Item = SyncTarget<'engine>>>) -> Self {
        let len = targets.size_hint().0.min(BATCH_LIMIT);
        let key_capacity = (len * 2).min(BATCH_LIMIT);
        let mut plan = Self {
            accounts: Vec::with_capacity(len),
            keys: Vec::with_capacity(key_capacity),
            subscriptions: Vec::with_capacity(key_capacity),
        };
        while let Some(mut target) = targets.next_if(|target| {
            let entries = if target.property == Readonly { 1 } else { 2 };
            plan.keys.len() + entries <= BATCH_LIMIT
        }) {
            let pubkey = target.accessor.pubkey();
            let index = plan.keys.len();
            let retention =
                if target.accessor.exists() { Retention::Reuse } else { Retention::Acquire };
            plan.keys.push(pubkey);
            if !target.subscribed && target.property != Writable {
                plan.subscriptions
                    .push((AccountSubscription { pubkey, program: None }, retention));
                target.subscribed = true;
            }
            let companion = match target.property {
                Payer | Writable => {
                    plan.keys.push(delegation_record_pda_from_delegated_account(&pubkey));
                    Some(Companion::Record(index + 1))
                }
                Program => {
                    let data = AccountSubscription::program_data(pubkey);
                    plan.keys.push(data.pubkey);
                    plan.subscriptions.push((data, retention));
                    Some(Companion::ProgramData(index + 1))
                }
                Readonly => None,
            };
            plan.accounts.push(PendingAccount { target, index, companion });
        }
        plan
    }
}
