use std::{collections::HashSet, fmt, sync::Arc};

use log::*;
use solana_pubkey::Pubkey;
use tokio::task::JoinSet;

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
    },
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
            } => {
                new_subs.is_empty()
                    && existing_subs.is_empty()
                    && all.is_empty()
            }
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
            } => write!(
                f,
                "Hybrid(New: {}, Existing: {}, All: {})",
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
                    .join(", ")
            ),
        }
    }
}

pub(crate) async fn cancel_subs<T: ChainRpcClient, U: ChainPubsubClient>(
    provider: &Arc<RemoteAccountProvider<T, U>>,
    strategy: CancelStrategy,
) {
    if strategy.is_empty() {
        trace!("No subscriptions to cancel");
        return;
    }
    let mut joinset = JoinSet::new();

    trace!("Canceling subscriptions with strategy: {strategy}");
    let subs_to_cancel = match strategy {
        CancelStrategy::All(pubkeys) => pubkeys,
        CancelStrategy::New {
            new_subs,
            existing_subs,
        } => new_subs.difference(&existing_subs).cloned().collect(),
        CancelStrategy::Hybrid {
            new_subs,
            existing_subs,
            all,
        } => new_subs
            .difference(&existing_subs)
            .cloned()
            .chain(all.into_iter())
            .collect(),
    };
    if log::log_enabled!(log::Level::Trace) {
        trace!(
            "Canceling subscriptions for: {}",
            subs_to_cancel
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    for pubkey in subs_to_cancel {
        let provider_clone = provider.clone();
        joinset.spawn(async move {
            // Check if there are pending requests for this account before unsubscribing
            // This prevents race conditions where one operation unsubscribes while another still needs it
            if provider_clone.is_pending(&pubkey) {
                debug!(
                    "Skipping unsubscribe for {pubkey} - has pending requests"
                );
                return;
            }

            if let Err(err) = provider_clone.unsubscribe(&pubkey).await {
                warn!("Failed to unsubscribe from {pubkey}: {err:?}");
            }
        });
    }

    joinset.join_all().await;
}
