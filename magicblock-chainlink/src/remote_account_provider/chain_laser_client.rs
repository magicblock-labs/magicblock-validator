#![allow(unused)]
use log::*;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use solana_pubkey::Pubkey;
use tokio::sync::{mpsc, oneshot};

use crate::remote_account_provider::{
    chain_laser_actor::ChainLaserActor,
    pubsub_common::{ChainPubsubActorMessage, SubscriptionUpdate},
    ChainPubsubClient, RemoteAccountProviderResult,
};

#[derive(Clone)]
pub struct ChainLaserClientImpl {
    actor: Arc<ChainLaserActor>,
    updates: Arc<Mutex<Option<mpsc::Receiver<SubscriptionUpdate>>>>,
}

#[async_trait]
impl ChainPubsubClient for ChainLaserClientImpl {
    async fn subscribe(
        &self,
        pubkey: Pubkey,
    ) -> RemoteAccountProviderResult<()> {
        let (tx, rx) = oneshot::channel();
        self.actor
            .send_msg(ChainPubsubActorMessage::AccountSubscribe {
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
        self.actor
            .send_msg(ChainPubsubActorMessage::AccountUnsubscribe {
                pubkey,
                response: tx,
            })
            .await?;

        rx.await?
    }

    async fn shutdown(&self) -> RemoteAccountProviderResult<()> {
        let (tx, rx) = oneshot::channel();
        self.actor
            .send_msg(ChainPubsubActorMessage::Shutdown { response: tx })
            .await?;
        rx.await?
    }

    async fn recycle_connections(&self) {
        trace!("No need to recycle laserstream connections");
    }

    fn take_updates(&self) -> mpsc::Receiver<SubscriptionUpdate> {
        let mut updates_lock = self.updates.lock().unwrap();
        updates_lock
            .take()
            .expect("ChainLaserClientImpl::take_updates called more than once")
    }
}
