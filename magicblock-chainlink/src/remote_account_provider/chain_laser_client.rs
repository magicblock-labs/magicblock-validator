use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use solana_pubkey::Pubkey;
use tokio::sync::{mpsc, oneshot};

use crate::remote_account_provider::{
    chain_laser_actor::ChainLaserActor,
    pubsub_common::{ChainPubsubActorMessage, SubscriptionUpdate},
    ChainPubsubClient, RemoteAccountProviderError, RemoteAccountProviderResult,
};

#[derive(Clone)]
pub struct ChainLaserClientImpl {
    /// Receiver for subscription updates
    updates: Arc<Mutex<Option<mpsc::Receiver<SubscriptionUpdate>>>>,
    /// Channel to send messages to the actor
    messages: mpsc::Sender<ChainPubsubActorMessage>,
}

impl ChainLaserClientImpl {
    pub async fn new_from_url(
        pubsub_url: &str,
        api_key: &str,
        commitment: solana_sdk::commitment_config::CommitmentLevel,
    ) -> RemoteAccountProviderResult<Self> {
        let (actor, messages, updates) =
            ChainLaserActor::new_from_url(pubsub_url, api_key, commitment)?;
        let client = Self {
            updates: Arc::new(Mutex::new(Some(updates))),
            messages,
        };
        tokio::spawn(actor.run());
        Ok(client)
    }

    async fn send_msg(
        &self,
        msg: ChainPubsubActorMessage,
    ) -> RemoteAccountProviderResult<()> {
        self.messages.send(msg).await.map_err(|err| {
            RemoteAccountProviderError::ChainLaserActorSendError(
                err.to_string(),
                format!("{err:#?}"),
            )
        })
    }
}

#[async_trait]
impl ChainPubsubClient for ChainLaserClientImpl {
    async fn subscribe(
        &self,
        pubkey: Pubkey,
    ) -> RemoteAccountProviderResult<()> {
        let (tx, rx) = oneshot::channel();
        self.send_msg(ChainPubsubActorMessage::AccountSubscribe {
            pubkey,
            response: tx,
        })
        .await?;

        rx.await?
    }

    async fn unsubscribe(
        &self,
        pubkey: Pubkey,
    ) -> RemoteAccountProviderResult<()> {
        let (tx, rx) = oneshot::channel();
        self.send_msg(ChainPubsubActorMessage::AccountUnsubscribe {
            pubkey,
            response: tx,
        })
        .await?;

        rx.await?
    }

    async fn shutdown(&self) -> RemoteAccountProviderResult<()> {
        // TODO(thlorenz): @@@ shutdown
        todo!("ChainLaserClientImpl::shutdown not implemented yet")
    }

    fn take_updates(&self) -> mpsc::Receiver<SubscriptionUpdate> {
        let mut updates_lock = self.updates.lock().unwrap();
        updates_lock
            .take()
            .expect("ChainLaserClientImpl::take_updates called more than once")
    }

    async fn subscription_count(
        &self,
        _exclude: Option<&[Pubkey]>,
    ) -> (usize, usize) {
        // TODO(thlorenz): @@@ implement subscription_count
        todo!("ChainLaserClientImpl::subscription_count not implemented yet")
    }

    fn subscriptions(&self) -> Vec<Pubkey> {
        // TODO(thlorenz): @@@ implement subscriptions
        todo!("ChainLaserClientImpl::subscriptions not implemented yet")
    }
}

pub fn is_helius_laser_url(url: &str) -> bool {
    // Example: https://laserstream-devnet-ewr.helius-rpc.com
    url.contains("laserstream") && url.contains("helius-rpc.com")
}
