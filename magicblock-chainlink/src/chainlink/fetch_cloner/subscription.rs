use std::sync::Arc;

use solana_pubkey::Pubkey;
use tracing::*;

use crate::{
    chainlink::errors::{ChainlinkError, ChainlinkResult},
    remote_account_provider::{
        ChainPubsubClient, ChainRpcClient, RemoteAccountProvider,
        SubscriptionReason,
    },
};

pub(crate) enum SubscriptionRelease {
    Pubkey {
        pubkey: Pubkey,
        reason: SubscriptionReason,
    },
}

pub(crate) async fn acquire_subs<T: ChainRpcClient, U: ChainPubsubClient>(
    provider: &Arc<RemoteAccountProvider<T, U>>,
    pubkeys: impl IntoIterator<Item = Pubkey>,
    reason: SubscriptionReason,
) -> ChainlinkResult<()> {
    let mut acquired = Vec::new();

    for pubkey in pubkeys {
        if let Err(err) = provider.acquire_subscription(&pubkey, reason).await {
            release_subs(
                provider,
                acquired.into_iter().map(|pubkey| {
                    SubscriptionRelease::Pubkey { pubkey, reason }
                }),
            )
            .await;
            return Err(ChainlinkError::FailedToSubscribeToAccount(
                pubkey, err,
            ));
        }
        acquired.push(pubkey);
    }

    Ok(())
}

#[instrument(skip(provider, releases))]
pub(crate) async fn release_subs<T: ChainRpcClient, U: ChainPubsubClient>(
    provider: &Arc<RemoteAccountProvider<T, U>>,
    releases: impl IntoIterator<Item = SubscriptionRelease>,
) {
    for release in releases {
        let SubscriptionRelease::Pubkey { pubkey, reason } = release;
        if let Err(err) =
            provider.release_single_subscription(&pubkey, reason).await
        {
            warn!(pubkey = %pubkey, ?reason, error = ?err, "Failed to release subscription reason");
        }
    }
}

pub(crate) async fn release_program_data_subs<
    T: ChainRpcClient,
    U: ChainPubsubClient,
>(
    provider: &Arc<RemoteAccountProvider<T, U>>,
    program_data_pubkey: Pubkey,
) {
    release_subs(
        provider,
        [
            SubscriptionRelease::Pubkey {
                pubkey: program_data_pubkey,
                reason: SubscriptionReason::DirectAccount,
            },
            SubscriptionRelease::Pubkey {
                pubkey: program_data_pubkey,
                reason: SubscriptionReason::ProgramData,
            },
        ],
    )
    .await;
}
