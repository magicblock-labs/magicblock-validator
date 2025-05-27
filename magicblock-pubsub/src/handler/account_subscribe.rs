use jsonrpc_pubsub::Subscriber;
use magicblock_geyser_plugin::rpc::GeyserRpcService;
use solana_account_decoder::UiAccountEncoding;
use solana_sdk::pubkey::Pubkey;

use crate::{
    errors::reject_internal_error,
    notification_builder::AccountNotificationBuilder, types::AccountParams,
};

use super::common::UpdateHandler;

pub async fn handle_account_subscribe(
    subid: u64,
    subscriber: Subscriber,
    params: &AccountParams,
    geyser_service: &GeyserRpcService,
) {
    let pubkey = match Pubkey::try_from(params.pubkey()) {
        Ok(pubkey) => pubkey,
        Err(err) => {
            reject_internal_error(subscriber, "Invalid Pubkey", Some(err));
            return;
        }
    };

    let mut geyser_rx = geyser_service.accounts_subscribe(subid, pubkey).await;

    let builder = AccountNotificationBuilder {
        encoding: params.encoding().unwrap_or(UiAccountEncoding::Base58),
    };
    let subscriptions_db = geyser_service.subscriptions_db.clone();
    let cleanup = async move {
        subscriptions_db
            .unsubscribe_from_account(&pubkey, subid)
            .await;
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
