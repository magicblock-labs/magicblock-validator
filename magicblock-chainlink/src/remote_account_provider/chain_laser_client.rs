use std::{
    collections::HashSet,
    sync::{atomic::AtomicU64, Arc, Mutex},
};

use async_trait::async_trait;
use log::*;
use solana_commitment_config::CommitmentLevel;
use solana_pubkey::Pubkey;
use tokio::sync::{mpsc, oneshot};

use crate::remote_account_provider::{
    chain_laser_actor::ChainLaserActor,
    pubsub_common::{ChainPubsubActorMessage, SubscriptionUpdate},
    ChainPubsubClient, ReconnectableClient, RemoteAccountProviderError,
    RemoteAccountProviderResult,
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
        commitment: CommitmentLevel,
        abort_sender: mpsc::Sender<()>,
        chain_slot: Arc<AtomicU64>,
    ) -> RemoteAccountProviderResult<Self> {
        let (actor, messages, updates) = ChainLaserActor::new_from_url(
            pubsub_url,
            api_key,
            commitment,
            abort_sender,
            chain_slot,
        )?;
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

    async fn subscribe_program(
        &self,
        program_id: Pubkey,
    ) -> RemoteAccountProviderResult<()> {
        let (tx, rx) = oneshot::channel();
        self.send_msg(ChainPubsubActorMessage::ProgramSubscribe {
            pubkey: program_id,
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
        let (tx, rx) = oneshot::channel();
        self.send_msg(ChainPubsubActorMessage::Shutdown { response: tx })
            .await?;

        rx.await.map_err(|err| {
            RemoteAccountProviderError::ChainLaserActorSendError(
                err.to_string(),
                format!("{err:#?}"),
            )
        })?
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
    ) -> Option<(usize, usize)> {
        None
    }

    fn subscriptions(&self) -> Option<Vec<Pubkey>> {
        None
    }

    fn subs_immediately(&self) -> bool {
        false
    }
}

#[async_trait]
impl ReconnectableClient for ChainLaserClientImpl {
    async fn try_reconnect(&self) -> RemoteAccountProviderResult<()> {
        let (tx, rx) = oneshot::channel();
        self.send_msg(ChainPubsubActorMessage::Reconnect { response: tx })
            .await?;

        rx.await.inspect_err(|err| {
            warn!("RecvError occurred while awaiting reconnect response: {err:?}.");
        })?
    }

    async fn resub_multiple(
        &self,
        pubkeys: HashSet<Pubkey>,
    ) -> RemoteAccountProviderResult<()> {
        // NOTE: The laser implementation subscribes periodically to requested accounts
        // thus we don't need to throttle the speed at which we resubscribe here.
        for pubkey in pubkeys {
            self.subscribe(pubkey).await?;
        }
        Ok(())
    }
}

pub fn is_known_grpc_url(url: &str) -> bool {
    is_helius_laser_url(url) || is_triton_url(url)
}

fn is_helius_laser_url(url: &str) -> bool {
    // Example: https://laserstream-devnet-ewr.helius-rpc.com
    url.contains("laserstream") && url.contains("helius-rpc.com")
}

fn is_triton_url(url: &str) -> bool {
    // Example: https://magicblo-dev<redacted>.devnet.rpcpool.com
    url.contains("rpcpool")
}
