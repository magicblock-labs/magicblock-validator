use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::commitment_config::CommitmentConfig;
use tokio::sync::{mpsc, oneshot};

use crate::{
    remote_account_provider::{
        chain_pubsub_actor::ChainPubsubActor,
        pubsub_common::{ChainPubsubActorMessage, SubscriptionUpdate},
    },
    testing::utils::{PUBSUB_URL, RPC_URL},
};

pub async fn setup_actor_and_client() -> (
    ChainPubsubActor,
    mpsc::Receiver<SubscriptionUpdate>,
    RpcClient,
) {
    let (tx, _) = mpsc::channel(10);
    let (actor, updates_rx) = ChainPubsubActor::new_from_url(
        PUBSUB_URL,
        tx,
        CommitmentConfig::confirmed(),
    )
    .await
    .expect("failed to create ChainPubsubActor");
    let rpc_client = RpcClient::new(RPC_URL.to_string());
    (actor, updates_rx, rpc_client)
}

pub async fn subscribe(actor: &ChainPubsubActor, pubkey: Pubkey) {
    let (tx, rx) = oneshot::channel();
    actor
        .send_msg(ChainPubsubActorMessage::AccountSubscribe {
            pubkey,
            response: tx,
        })
        .await
        .expect("failed to send AccountSubscribe message");
    rx.await
        .expect("subscribe ack channel dropped")
        .expect("subscribe failed");
}

pub async fn unsubscribe(actor: &ChainPubsubActor, pubkey: Pubkey) {
    let (tx, rx) = oneshot::channel();
    actor
        .send_msg(ChainPubsubActorMessage::AccountUnsubscribe {
            pubkey,
            response: tx,
        })
        .await
        .expect("failed to send AccountUnsubscribe message");
    rx.await
        .expect("unsubscribe ack channel dropped")
        .expect("unsubscribe failed");
}

pub async fn reconnect(actor: &ChainPubsubActor) {
    let (tx, rx) = oneshot::channel();
    actor
        .send_msg(ChainPubsubActorMessage::Reconnect { response: tx })
        .await
        .expect("failed to send Reconnect message");
    rx.await
        .expect("reconnect ack channel dropped")
        .expect("reconnect failed");
}

pub async fn shutdown(actor: &ChainPubsubActor) {
    actor.shutdown().await;
}
