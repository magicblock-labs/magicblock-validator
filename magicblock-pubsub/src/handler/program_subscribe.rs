use jsonrpc_pubsub::Subscriber;
use magicblock_geyser_plugin::rpc::GeyserRpcService;
use solana_account_decoder::UiAccountEncoding;
use solana_sdk::pubkey::Pubkey;

use super::common::UpdateHandler;
use crate::{
    errors::reject_internal_error,
    notification_builder::{ProgramFilters, ProgramNotificationBuilder},
    types::ProgramParams,
};

pub async fn handle_program_subscribe(
    subid: u64,
    subscriber: Subscriber,
    params: &ProgramParams,
    geyser_service: &GeyserRpcService,
) {
    let address = params.program_id();
    let config = params.config().clone().unwrap_or_default();

    let pubkey = match Pubkey::try_from(address) {
        Ok(pubkey) => pubkey,
        Err(err) => {
            reject_internal_error(subscriber, "Invalid Pubkey", Some(err));
            return;
        }
    };

    let mut geyser_rx = geyser_service.program_subscribe(subid, pubkey).await;

    let encoding = config
        .account_config
        .encoding
        .unwrap_or(UiAccountEncoding::Base58);
    let filters = ProgramFilters::from(config.filters);
    let builder = ProgramNotificationBuilder { encoding, filters };
    let subscriptions_db = geyser_service.subscriptions_db.clone();
    let cleanup = async move {
        subscriptions_db
            .unsubscribe_from_program(&pubkey, subid)
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
