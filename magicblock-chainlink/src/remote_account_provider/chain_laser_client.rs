use async_trait::async_trait;
use solana_pubkey::Pubkey;
use tokio::sync::mpsc;

use crate::remote_account_provider::{
    pubsub_common::SubscriptionUpdate, ChainPubsubClient,
    RemoteAccountProviderResult,
};

#[derive(Clone)]
pub struct ChainLaserClientImpl {}

#[async_trait]
impl ChainPubsubClient for ChainLaserClientImpl {
    async fn subscribe(
        &self,
        _pubkey: Pubkey,
    ) -> RemoteAccountProviderResult<()> {
        unimplemented!()
    }

    async fn unsubscribe(
        &self,
        _pubkey: Pubkey,
    ) -> RemoteAccountProviderResult<()> {
        unimplemented!()
    }

    async fn shutdown(&self) {
        unimplemented!()
    }

    async fn recycle_connections(&self) {
        unimplemented!()
    }

    fn take_updates(&self) -> mpsc::Receiver<SubscriptionUpdate> {
        unimplemented!()
    }
}
