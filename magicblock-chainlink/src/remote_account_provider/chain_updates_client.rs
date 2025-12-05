use std::collections::HashSet;

use async_trait::async_trait;
use log::*;
use solana_pubkey::Pubkey;
use solana_sdk::commitment_config::CommitmentConfig;
use tokio::sync::mpsc;

use crate::remote_account_provider::{
    chain_laser_client::{is_helius_laser_url, ChainLaserClientImpl},
    pubsub_common::SubscriptionUpdate,
    ChainPubsubClient, ChainPubsubClientImpl, Endpoint,
    RemoteAccountProviderError, RemoteAccountProviderResult,
    ReconnectableClient,
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
        let me = if is_helius_laser_url(&endpoint.pubsub_url) {
            let (url, api_key) = endpoint.separate_pubsub_url_and_api_key();
            let Some(api_key) = api_key else {
                return Err(RemoteAccountProviderError::MissingApiKey(
                    format!("Helius laser endpoint: {}", endpoint.pubsub_url),
                ));
            };
            debug!(
                "Initializing Helius Laser client for endpoint: {}",
                endpoint.pubsub_url
            );
            ChainUpdatesClient::Laser(
                ChainLaserClientImpl::new_from_url(
                    &url,
                    &api_key,
                    commitment.commitment,
                )
                .await?,
            )
        } else {
            debug!(
                "Initializing WebSocket client for endpoint: {}",
                endpoint.pubsub_url
            );
            ChainUpdatesClient::WebSocket(
                ChainPubsubClientImpl::try_new_from_url(
                    &endpoint.pubsub_url,
                    abort_sender,
                    commitment,
                )
                .await?,
            )
        };
        Ok(me)
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
    ) -> (usize, usize) {
        use ChainUpdatesClient::*;
        match self {
            WebSocket(client) => client.subscription_count(exclude).await,
            Laser(client) => client.subscription_count(exclude).await,
        }
    }

    fn subscriptions(&self) -> Vec<Pubkey> {
        use ChainUpdatesClient::*;
        match self {
            WebSocket(client) => client.subscriptions(),
            Laser(client) => client.subscriptions(),
        }
    }
}

#[async_trait]
impl ReconnectableClient for ChainUpdatesClient {
    async fn try_reconnect(&self) -> RemoteAccountProviderResult<()> {
        use ChainUpdatesClient::*;
        match self {
            WebSocket(client) => client.try_reconnect().await,
            Laser(_client) => {
                // TODO: implement reconnect for Laser client
                Ok(())
            }
        }
    }

    async fn resub_multiple(
        &self,
        pubkeys: HashSet<Pubkey>,
    ) -> RemoteAccountProviderResult<()> {
        use ChainUpdatesClient::*;
        match self {
            WebSocket(client) => client.resub_multiple(pubkeys).await,
            Laser(_client) => {
                // TODO: implement resub_multiple for Laser client
                Ok(())
            }
        }
    }
}
