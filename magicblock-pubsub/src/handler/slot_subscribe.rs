use jsonrpc_pubsub::Subscriber;
use magicblock_geyser_plugin::rpc::GeyserRpcService;

use super::common::UpdateHandler;
use crate::notification_builder::SlotNotificationBuilder;

pub async fn handle_slot_subscribe(
    subid: u64,
    subscriber: Subscriber,
    geyser_service: &GeyserRpcService,
) {
    let mut geyser_rx = geyser_service.slot_subscribe(subid).await;

    let builder = SlotNotificationBuilder {};
    let subscriptions_db = geyser_service.subscriptions_db.clone();
    let cleanup = async move {
        subscriptions_db.unsubscribe_from_slot(subid).await;
    };
    let Some(handler) =
        UpdateHandler::new(subid, subscriber, builder, cleanup.into())
    else {
        return;
    };
    while let Some(msg) = geyser_rx.recv().await {
        if !handler.handle_slot_update(msg) {
            break;
        }
    }
}
