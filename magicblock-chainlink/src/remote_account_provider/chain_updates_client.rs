use std::collections::HashSet;

use async_trait::async_trait;
use log::*;
use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;
use tokio::sync::mpsc;

use crate::remote_account_provider::{
    chain_laser_client::{is_helius_laser_url, ChainLaserClientImpl},
    pubsub_common::SubscriptionUpdate,
    ChainPubsubClient, ChainPubsubClientImpl, Endpoint, ReconnectableClient,
    RemoteAccountProviderError, RemoteAccountProviderResult,
};

#[derive(Clone)]
pub enum ChainUpdatesClient {
    WebSocket(ChainPubsubClientImpl),
    Laser(ChainLaserClientImpl),
}

impl ChainUpdatesClient {
    pub async fn try_new_from_endpoint(
        endpoint: &Endpoint,
        commitment: CommitmentConfig,
        abort_sender: mpsc::Sender<()>,
    ) -> RemoteAccountProviderResult<Self> {
        use Endpoint::*;
        match endpoint {
            WebSocket { url } => {
                debug!("Initializing WebSocket client for endpoint: {}", url);
                Ok(ChainUpdatesClient::WebSocket(
                    ChainPubsubClientImpl::try_new_from_url(
                        url,
                        abort_sender,
                        commitment,
                    )
                    .await?,
                ))
            }
            Grpc { url, api_key } => {
                debug!(
                    "Initializing Helius Laser client for gRPC endpoint: {}",
                    url
                );
                if is_helius_laser_url(url) {
                    Ok(ChainUpdatesClient::Laser(
                        ChainLaserClientImpl::new_from_url(
                            url,
                            api_key,
                            commitment.commitment,
                            abort_sender,
                        )
                        .await?,
                    ))
                } else {
                    Err(RemoteAccountProviderError::UnsupportedGrpcEndpoint(
                        url.to_string(),
                    ))
                }
            }
            Rpc { .. } => {
                Err(RemoteAccountProviderError::InvalidPubsubEndpoint(format!(
                    "{endpoint:?}"
                )))
            }
        }
    }
}

#[async_trait]
impl ChainPubsubClient for ChainUpdatesClient {
    async fn subscribe(
        &self,
        pubkey: Pubkey,
    ) -> RemoteAccountProviderResult<()> {
        use ChainUpdatesClient::*;
        match self {
            WebSocket(client) => client.subscribe(pubkey).await,
            Laser(client) => client.subscribe(pubkey).await,
        }
    }

    async fn subscribe_program(
        &self,
        program_id: Pubkey,
    ) -> RemoteAccountProviderResult<()> {
        use ChainUpdatesClient::*;
        match self {
            WebSocket(client) => client.subscribe_program(program_id).await,
            Laser(client) => client.subscribe_program(program_id).await,
        }
    }

    async fn unsubscribe(
        &self,
        pubkey: Pubkey,
    ) -> RemoteAccountProviderResult<()> {
        use ChainUpdatesClient::*;
        match self {
            WebSocket(client) => client.unsubscribe(pubkey).await,
            Laser(client) => client.unsubscribe(pubkey).await,
        }
    }

    async fn shutdown(&self) -> RemoteAccountProviderResult<()> {
        use ChainUpdatesClient::*;
        match self {
            WebSocket(client) => client.shutdown().await,
            Laser(client) => client.shutdown().await,
        }
    }

    fn take_updates(&self) -> mpsc::Receiver<SubscriptionUpdate> {
        use ChainUpdatesClient::*;
        match self {
            WebSocket(client) => client.take_updates(),
            Laser(client) => client.take_updates(),
        }
    }

    /// Provides the total number of subscriptions and the number of
    /// subscriptions when excludig pubkeys in `exclude`.
    /// - `exclude`: Optional slice of pubkeys to exclude from the count.
    /// Returns a tuple of (total subscriptions, filtered subscriptions).
    async fn subscription_count(
        &self,
        exclude: Option<&[Pubkey]>,
    ) -> Option<(usize, usize)> {
        use ChainUpdatesClient::*;
        match self {
            WebSocket(client) => client.subscription_count(exclude).await,
            Laser(client) => client.subscription_count(exclude).await,
        }
    }

    fn subscriptions(&self) -> Option<Vec<Pubkey>> {
        use ChainUpdatesClient::*;
        match self {
            WebSocket(client) => client.subscriptions(),
            Laser(client) => client.subscriptions(),
        }
    }

    fn subs_immediately(&self) -> bool {
        use ChainUpdatesClient::*;
        match self {
            WebSocket(client) => client.subs_immediately(),
            Laser(client) => client.subs_immediately(),
        }
    }
}

#[async_trait]
impl ReconnectableClient for ChainUpdatesClient {
    async fn try_reconnect(&self) -> RemoteAccountProviderResult<()> {
        use ChainUpdatesClient::*;
        match self {
            WebSocket(client) => client.try_reconnect().await,
            Laser(client) => client.try_reconnect().await,
        }
    }

    async fn resub_multiple(
        &self,
        pubkeys: HashSet<Pubkey>,
    ) -> RemoteAccountProviderResult<()> {
        use ChainUpdatesClient::*;
        match self {
            WebSocket(client) => client.resub_multiple(pubkeys).await,
            Laser(client) => client.resub_multiple(pubkeys).await,
        }
    }
}
