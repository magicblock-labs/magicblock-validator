use std::collections::HashSet;

use log::*;
use solana_pubkey::Pubkey;

use super::{AccountsLruCache, ChainPubsubClient};

/// Reconciles subscription state between the LRU cache and the pubsub client.
///
/// This function is called when a mismatch is detected between the accounts
/// tracked in the LRU cache and the actual subscriptions held by the pubsub
/// client. It ensures both are in sync by:
///
/// - **Resubscribing**: Accounts present in the LRU cache but missing from the
///   pubsub client are resubscribed. This can happen if subscriptions were
///   dropped due to network issues or reconnections.
///
/// - **Unsubscribing**: Accounts present in the pubsub client but missing from
///   the LRU cache are unsubscribed. This can happen if the LRU evicted an
///   account but the unsubscribe request failed or was lost.
///
/// # Parameters
///
/// - `subscribed_accounts`: The LRU cache that tracks which accounts should be
///   subscribed. This is the source of truth for user-requested subscriptions.
///
/// - `pubsub_client`: The client managing actual WebSocket/gRPC subscriptions
///   to the chain. Provides the current set of active subscriptions.
///
/// - `never_evicted`: A list of system accounts (e.g., sysvar::clock) that are
///   always subscribed but are **not** tracked in the LRU cache. These accounts
///   are excluded from reconciliation because:
///   1. They are subscribed directly without going through the LRU.
///   2. They should never be unsubscribed regardless of LRU state.
///   3. Their presence in pubsub but absence from LRU is expected and correct.
///
///   Without filtering these out, the reconciler would incorrectly attempt to
///   unsubscribe them since they appear in pubsub but not in the LRU cache.
pub async fn reconcile_subscriptions<PubsubClient: ChainPubsubClient>(
    subscribed_accounts: &AccountsLruCache,
    pubsub_client: &PubsubClient,
    never_evicted: &[Pubkey],
) {
    let all_pubsub_subs = pubsub_client.subscriptions().unwrap_or_default();
    let lru_pubkeys = subscribed_accounts.pubkeys();

    let pubsub_subs_without_never_evict: HashSet<_> = all_pubsub_subs
        .iter()
        .filter(|pk| !never_evicted.contains(pk))
        .copied()
        .collect();
    let lru_pubkeys_set: HashSet<_> = lru_pubkeys.into_iter().collect();

    // A) LRU has more subscriptions than pubsub - need to resubscribe
    let extra_in_lru: Vec<_> = lru_pubkeys_set
        .difference(&pubsub_subs_without_never_evict)
        .cloned()
        .collect();

    // B) Pubsub has more subscriptions than LRU - need to unsubscribe
    let extra_in_pubsub: Vec<_> = pubsub_subs_without_never_evict
        .difference(&lru_pubkeys_set)
        .cloned()
        .collect();

    if !extra_in_lru.is_empty() {
        debug!(
            "Resubscribing {} accounts in LRU but not in pubsub: {:?}",
            extra_in_lru.len(),
            extra_in_lru
        );
        for pubkey in extra_in_lru {
            if let Err(e) = pubsub_client.subscribe(pubkey).await {
                warn!("Failed to resubscribe account {}: {}", pubkey, e);
            }
        }
    }

    if !extra_in_pubsub.is_empty() {
        debug!(
            "Unsubscribing {} accounts in pubsub but not in LRU: {:?}",
            extra_in_pubsub.len(),
            extra_in_pubsub
        );
        for pubkey in extra_in_pubsub {
            if let Err(e) = pubsub_client.unsubscribe(pubkey).await {
                warn!("Failed to unsubscribe account {}: {}", pubkey, e);
            }
        }
    }
}
