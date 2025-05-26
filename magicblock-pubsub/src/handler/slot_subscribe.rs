use jsonrpc_pubsub::Subscriber;
use magicblock_geyser_plugin::rpc::GeyserRpcService;

use crate::notification_builder::SlotNotificationBuilder;

use super::common::UpdateHandler;

pub async fn handle_slot_subscribe(
    subid: u64,
    subscriber: Subscriber,
    geyser_service: &GeyserRpcService,
) {
    let mut geyser_rx = geyser_service.slot_subscribe(subid);

    let builder = SlotNotificationBuilder {};
    let subscriptions_db = geyser_service.subscriptions_db.clone();
    let cleanup = move || {
        subscriptions_db.unsubscribe_from_slot(subid);
    };
    let Some(handler) = UpdateHandler::new(subid, subscriber, builder, cleanup)
    else {
        return;
    };
    while let Some(msg) = geyser_rx.recv().await {
        if !handler.handle(msg) {
            break;
        }
    }
}
