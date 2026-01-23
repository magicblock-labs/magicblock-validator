use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use solana_commitment_config::CommitmentLevel;
use solana_pubkey::Pubkey;
use solana_sdk_ids::sysvar::clock;
use tokio::sync::{mpsc, oneshot};
use tracing::*;

use crate::remote_account_provider::{
    chain_laser_actor::{ChainLaserActor, Slots},
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
    /// Client identifier
    client_id: String,
}

impl ChainLaserClientImpl {
    pub async fn new_from_url(
        pubsub_url: &str,
        client_id: String,
        api_key: &str,
        commitment: CommitmentLevel,
        abort_sender: mpsc::Sender<()>,
        slots: Slots,
    ) -> RemoteAccountProviderResult<Self> {
        let (actor, messages, updates) = ChainLaserActor::new_from_url(
            pubsub_url,
            &client_id,
            api_key,
            commitment,
            abort_sender,
            slots,
        )?;
        let client = Self {
            updates: Arc::new(Mutex::new(Some(updates))),
            messages,
            client_id,
        };
        tokio::spawn(actor.run());
        Ok(client)
    }

    #[instrument(skip(self, msg), fields(client_id = %self.client_id))]
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
        // Skip clock::ID subscriptions for GRPC clients since they get slot
        // updates directly via the SubscribeRequestFilterSlots in the GRPC
        // subscription request. This avoids redundant subscriptions.
        if pubkey == clock::ID {
            return Ok(());
        }

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

    fn id(&self) -> &str {
        &self.client_id
    }
}

#[async_trait]
impl ReconnectableClient for ChainLaserClientImpl {
    async fn try_reconnect(&self) -> RemoteAccountProviderResult<()> {
        let (tx, rx) = oneshot::channel();
        self.send_msg(ChainPubsubActorMessage::Reconnect { response: tx })
            .await?;

        rx.await.inspect_err(|err| {
            warn!(error = ?err, "RecvError while awaiting reconnect response");
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
