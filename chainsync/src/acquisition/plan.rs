//! Ordered primary/companion fetch layouts for targets already selected under leases.

use std::iter::Peekable;

use dlp_api::pda::delegation_record_pda_from_delegated_account;
use solana_pubkey::Pubkey;

use crate::{
    AccountProperty::{Payer, Program, Readonly, Writable},
    AccountSubscription, SyncTarget,
};

/// One selected target and its position in an ordered HTTP batch.
pub(super) struct PendingAccount<'engine> {
    /// Owns the lease and primary subscription across companion fetches.
    pub(super) target: SyncTarget<'engine>,
    /// Primary position in `FetchPlan::keys`; any companion immediately follows it.
    pub(super) index: usize,
}

/// One RPC-sized batch, including new subscriptions to acknowledge before fetching.
pub(super) struct FetchPlan<'engine> {
    /// Selected targets in the same order as their primary response positions.
    pub(super) accounts: Vec<PendingAccount<'engine>>,
    /// Primary and companion remote addresses in HTTP response order.
    pub(super) keys: Vec<Pubkey>,
    /// Subscription requests and whether to reuse a resident mirror's existing reference.
    pub(super) subscriptions: Vec<(AccountSubscription, bool)>,
}

impl<'engine> FetchPlan<'engine> {
    /// Takes the next targets without exceeding the 100-entry RPC limit with companions.
    pub(super) fn next(targets: &mut Peekable<impl Iterator<Item = SyncTarget<'engine>>>) -> Self {
        let len = targets.size_hint().0.min(100);
        let mut plan = Self {
            accounts: Vec::with_capacity(len),
            keys: Vec::with_capacity((len * 2).min(100)),
            subscriptions: Vec::with_capacity((len * 2).min(100)),
        };
        while let Some(mut target) = targets
            .next_if(|target| plan.keys.len() + 1 + usize::from(target.property != Readonly) <= 100)
        {
            let pubkey = target.accessor.pubkey();
            let index = plan.keys.len();
            plan.keys.push(pubkey);
            if !target.subscribed && target.property != Writable {
                plan.subscriptions.push((
                    AccountSubscription { pubkey, program: None },
                    target.accessor.exists(),
                ));
                target.subscribed = true;
            }
            match target.property {
                Payer | Writable => {
                    plan.keys.push(delegation_record_pda_from_delegated_account(&pubkey))
                }
                Program => {
                    let data = AccountSubscription::program_data(pubkey);
                    plan.keys.push(data.pubkey);
                    plan.subscriptions.push((data, target.accessor.exists()));
                }
                Readonly => {}
            }
            plan.accounts.push(PendingAccount { target, index });
        }
        plan
    }
}
