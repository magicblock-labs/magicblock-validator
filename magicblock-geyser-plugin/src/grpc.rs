// Adapted yellowstone-grpc/yellowstone-grpc-geyser/src/grpc.rs

use crate::{
    grpc_messages::*,
    types::{GeyserMessageReceiver, SubscriptionsDb},
};

#[derive(Debug)]
pub struct GrpcService {}

impl GrpcService {
    pub(crate) async fn geyser_loop(
        messages_rx: GeyserMessageReceiver,
        subscriptions_db: SubscriptionsDb,
    ) {
        while let Ok(message) = messages_rx.recv_async().await {
            match *message {
                Message::Slot(_) => {
                    subscriptions_db.send_slot(message).await;
                }
                Message::Account(ref account) => {
                    let pubkey = account.account.pubkey;
                    let owner = account.account.owner;
                    subscriptions_db
                        .send_account_update(&pubkey, message.clone())
                        .await;
                    subscriptions_db.send_program_update(&owner, message).await;
                }
                Message::Transaction(ref txn) => {
                    let signature = txn.transaction.signature;
                    subscriptions_db
                        .send_signature_update(&signature, message.clone())
                        .await;
                    subscriptions_db.send_logs_update(message).await;
                }
                Message::Block(_) => {}
                _ => (),
            }
        }
    }
}
