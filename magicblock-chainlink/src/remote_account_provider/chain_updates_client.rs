use async_trait::async_trait;
use solana_pubkey::Pubkey;
use solana_sdk::commitment_config::CommitmentConfig;

use crate::remote_account_provider::{
    chain_laser_client::{is_helius_laser_url, ChainLaserClientImpl},
    pubsub_common::SubscriptionUpdate,
    ChainPubsubClient, ChainPubsubClientImpl, Endpoint,
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
    ) -> RemoteAccountProviderResult<Self> {
        let me = if is_helius_laser_url(&endpoint.pubsub_url) {
            let (url, api_key) = endpoint.separate_pubsub_url_and_api_key();
            let Some(api_key) = api_key else {
                return Err(RemoteAccountProviderError::MissingApiKey(
                    format!("Helius laser endpoint: {}", endpoint.pubsub_url),
                ));
            };
            ChainUpdatesClient::Laser(
                ChainLaserClientImpl::new_from_url(
                    &url,
                    &api_key,
                    commitment.commitment,
                )
                .await?,
            )
        } else {
            ChainUpdatesClient::WebSocket(
                ChainPubsubClientImpl::try_new_from_url(
                    &endpoint.pubsub_url,
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

    async fn recycle_connections(&self) {
        use ChainUpdatesClient::*;
        match self {
            WebSocket(client) => client.recycle_connections().await,
            Laser(client) => client.recycle_connections().await,
        }
    }

    fn take_updates(&self) -> tokio::sync::mpsc::Receiver<SubscriptionUpdate> {
        use ChainUpdatesClient::*;
        match self {
            WebSocket(client) => client.take_updates(),
            Laser(client) => client.take_updates(),
        }
    }
}
