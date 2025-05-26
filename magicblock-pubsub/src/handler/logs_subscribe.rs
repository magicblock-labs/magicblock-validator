use jsonrpc_pubsub::Subscriber;
use magicblock_geyser_plugin::{
    rpc::GeyserRpcService, types::LogsSubscribeKey,
};
use solana_rpc_client_api::config::RpcTransactionLogsFilter;
use solana_sdk::pubkey::Pubkey;

use crate::{
    errors::reject_internal_error,
    notification_builder::LogsNotificationBulider, types::LogsParams,
};

use super::common::UpdateHandler;

pub async fn handle_logs_subscribe(
    subid: u64,
    subscriber: Subscriber,
    params: &LogsParams,
    geyser_service: &GeyserRpcService,
) {
    let key = match params.filter() {
        RpcTransactionLogsFilter::All
        | RpcTransactionLogsFilter::AllWithVotes => LogsSubscribeKey::All,
        RpcTransactionLogsFilter::Mentions(pubkeys) => {
            let Some(Ok(pubkey)) =
                pubkeys.first().map(|s| Pubkey::try_from(s.as_str()))
            else {
                reject_internal_error(
                    subscriber,
                    "Invalid Pubkey",
                    Some("failed to base58 decode the provided pubkey"),
                );
                return;
            };
            LogsSubscribeKey::Account(pubkey)
        }
    };
    let mut geyser_rx = geyser_service.logs_subscribe(key, subid);
    let builder = LogsNotificationBulider {};
    let subscriptions_db = geyser_service.subscriptions_db.clone();
    let cleanup = move || {
        subscriptions_db.unsubscribe_from_logs(&key, subid);
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
