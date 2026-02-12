use std::collections::HashSet;

use solana_pubkey::Pubkey;
use tokio::sync::mpsc;
use tracing::*;

use super::{AccountsLruCache, ChainPubsubClient};

/// Unsubscribes from pubsub and sends a removal notification to trigger bank
/// removal.
///
/// This is the core logic shared between:
/// - Normal unsubscribe flow (after removing from LRU cache)
/// - Reconciliation flow (account missing from LRU cache)
#[instrument(skip(pubsub_client, removed_account_tx), fields(pubkey = %pubkey))]
pub(crate) async fn unsubscribe_and_notify_removal<T: ChainPubsubClient>(
    pubkey: Pubkey,
    pubsub_client: &T,
    removed_account_tx: &mpsc::Sender<Pubkey>,
) -> bool {
    match pubsub_client.unsubscribe(pubkey).await {
        Ok(()) => {
            if let Err(err) = removed_account_tx.send(pubkey).await {
                warn!(error = ?err, "Failed to send removal update");
            }
            true
        }
        Err(err) => {
            warn!(error = ?err, "Failed to unsubscribe");
            false
        }
    }
}

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
///
/// - `removed_account_tx`: Channel to notify upstream that an account was
///   unsubscribed and should be removed from the bank.
pub async fn reconcile_subscriptions<PubsubClient: ChainPubsubClient>(
    subscribed_accounts: &AccountsLruCache,
    pubsub_client: &PubsubClient,
    never_evicted: &[Pubkey],
    removed_account_tx: &mpsc::Sender<Pubkey>,
) {
    let pubsub_union = pubsub_client.subscriptions_union();
    let pubsub_intersection = pubsub_client.subscriptions_intersection();
    let lru_pubkeys = subscribed_accounts.pubkeys();

    let ensured_subs_without_never_evict: HashSet<_> = pubsub_intersection
        .into_iter()
        .filter(|pk| !never_evicted.contains(pk))
        .collect();
    let partial_subs_without_never_evict: HashSet<_> = pubsub_union
        .into_iter()
        .filter(|pk| !never_evicted.contains(pk))
        .collect();
    let lru_pubkeys_set: HashSet<_> = lru_pubkeys.into_iter().collect();

    // A) LRU subs that are not ensured by all clients
    let extra_in_lru: Vec<_> = lru_pubkeys_set
        .difference(&ensured_subs_without_never_evict)
        .collect();
    // B) Subs not in LRU that some clients are subscribed to
    let extra_in_pubsub: Vec<_> = partial_subs_without_never_evict
        .difference(&ensured_subs_without_never_evict)
        .collect();

    // For any sub that is in the LRU but not ensured by all clients we resubscribe.
    // This may call subscribe on some clients that already have the subscription and
    // is ignored by that client.
    if !extra_in_lru.is_empty() {
        debug!(
            count = extra_in_lru.len(),
            "Resubscribing accounts in LRU but not in pubsub"
        );
        for pubkey in extra_in_lru {
            if let Err(e) = pubsub_client.subscribe(*pubkey, None).await {
                warn!(pubkey = %pubkey, error = ?e, "Failed to resubscribe account");
            }
        }
    }

    // For any sub that is in any client but not in the LRU we unsubscribe and trigger a removal
    // notification.
    // This may call unsubscribe on some clients that don't have the subscription and
    // is ignored by that client.
    if !extra_in_pubsub.is_empty() {
        debug!(
            count = extra_in_pubsub.len(),
            "Unsubscribing accounts in pubsub but not in LRU"
        );
        for pubkey in extra_in_pubsub {
            unsubscribe_and_notify_removal(
                *pubkey,
                pubsub_client,
                removed_account_tx,
            )
            .await;
        }
    }
}
