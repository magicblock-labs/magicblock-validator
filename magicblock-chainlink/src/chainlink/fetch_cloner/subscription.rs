use std::{
    collections::{HashMap, HashSet},
    fmt,
    sync::Arc,
};

use magicblock_core::traits::PhotonClient;
use solana_pubkey::Pubkey;
use tokio::task::JoinSet;
use tracing::*;

use crate::remote_account_provider::{
    ChainPubsubClient, ChainRpcClient, RemoteAccountProvider,
};

pub(crate) enum CancelStrategy {
    /// Cancel all subscriptions for the given pubkeys
    All(HashSet<Pubkey>),
    /// Cancel subscriptions for new accounts that are not in existing subscriptions
    New {
        new_subs: HashSet<Pubkey>,
        existing_subs: HashSet<Pubkey>,
    },
    /// Cancel subscriptions for new accounts that are not in existing subscriptions
    /// and also cancel all subscriptions for the given pubkeys in `all`
    Hybrid {
        new_subs: HashSet<Pubkey>,
        existing_subs: HashSet<Pubkey>,
        all: HashSet<Pubkey>,
        only_if_unchanged: HashMap<Pubkey, u64>,
    },
    /// Cancel subscriptions for accounts that are explicitly subscribed to
    /// and have not changed since the last snapshot.
    OnlyIfUnchanged { pubkeys: HashMap<Pubkey, u64> },
}

impl CancelStrategy {
    pub(crate) fn is_empty(&self) -> bool {
        match self {
            CancelStrategy::All(pubkeys) => pubkeys.is_empty(),
            CancelStrategy::New {
                new_subs,
                existing_subs,
            } => new_subs.is_empty() && existing_subs.is_empty(),
            CancelStrategy::Hybrid {
                new_subs,
                existing_subs,
                all,
                only_if_unchanged,
            } => {
                new_subs.is_empty()
                    && existing_subs.is_empty()
                    && all.is_empty()
                    && only_if_unchanged.is_empty()
            }
            CancelStrategy::OnlyIfUnchanged { pubkeys } => pubkeys.is_empty(),
        }
    }
}

impl fmt::Display for CancelStrategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CancelStrategy::All(pubkeys) => write!(
                f,
                "All({})",
                pubkeys
                    .iter()
                    .map(|p| p.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            CancelStrategy::New {
                new_subs,
                existing_subs,
            } => write!(
                f,
                "New({}) Existing({})",
                new_subs
                    .iter()
                    .map(|p| p.to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
                existing_subs
                    .iter()
                    .map(|p| p.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            CancelStrategy::Hybrid {
                new_subs,
                existing_subs,
                all,
                only_if_unchanged,
            } => write!(
                f,
                "Hybrid(New: {}, Existing: {}, All: {}, OnlyIfUnchanged: {})",
                new_subs
                    .iter()
                    .map(|p| p.to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
                existing_subs
                    .iter()
                    .map(|p| p.to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
                all.iter()
                    .map(|p| p.to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
                only_if_unchanged
                    .keys()
                    .map(|p| p.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            CancelStrategy::OnlyIfUnchanged { pubkeys } => write!(
                f,
                "OnlyIfUnchanged({})",
                pubkeys
                    .keys()
                    .map(|p| p.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

#[instrument(skip(provider, strategy))]
pub(crate) async fn cancel_subs<
    T: ChainRpcClient,
    U: ChainPubsubClient,
    P: PhotonClient,
>(
    provider: &Arc<RemoteAccountProvider<T, U, P>>,
    strategy: CancelStrategy,
) {
    if strategy.is_empty() {
        trace!("No subscriptions to cancel");
        return;
    }
    let mut joinset = JoinSet::new();

    trace!("Canceling subscriptions");
    let subs_to_cancel: HashMap<Pubkey, Option<u64>> = match strategy {
        CancelStrategy::All(pubkeys) => {
            pubkeys.into_iter().map(|pubkey| (pubkey, None)).collect()
        }
        CancelStrategy::New {
            new_subs,
            existing_subs,
        } => new_subs
            .difference(&existing_subs)
            .map(|pubkey| (*pubkey, None))
            .collect(),
        CancelStrategy::Hybrid {
            new_subs,
            existing_subs,
            all,
            only_if_unchanged,
        } => new_subs
            .difference(&existing_subs)
            .map(|pubkey| (*pubkey, None))
            .chain(all.into_iter().map(|pubkey| (pubkey, None)))
            .chain(
                only_if_unchanged
                    .into_iter()
                    .map(|(pubkey, generation)| (pubkey, Some(generation))),
            )
            .collect(),
        CancelStrategy::OnlyIfUnchanged { pubkeys } => pubkeys
            .into_iter()
            .map(|(pubkey, generation)| (pubkey, Some(generation)))
            .collect(),
    };
    if tracing::enabled!(tracing::Level::TRACE) {
        let pubkeys_str = subs_to_cancel
            .iter()
            .map(|(p, _)| p.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        trace!(pubkeys = %pubkeys_str, "Canceling subscriptions for");
    }

    for (pubkey, expected_generation) in subs_to_cancel {
        let provider_clone = provider.clone();
        joinset.spawn(async move {
            // Check if there are pending requests for this account before unsubscribing
            // This prevents race conditions where one operation unsubscribes while another still needs it
            if provider_clone.is_pending(&pubkey) {
                debug!(pubkey = %pubkey, "Skipping unsubscribe - has pending requests");
                return;
            }
            if let Some(expected_generation) = expected_generation {
                if provider_clone.subscription_generation(&pubkey)
                    != expected_generation
                {
                    debug!(pubkey = %pubkey, "Skipping unsubscribe - subscription changed");
                    return;
                }
            }

            if let Err(err) = provider_clone.unsubscribe(&pubkey).await {
                warn!(pubkey = %pubkey, error = ?err, "Failed to unsubscribe");
            }
        });
    }

    joinset.join_all().await;
}
