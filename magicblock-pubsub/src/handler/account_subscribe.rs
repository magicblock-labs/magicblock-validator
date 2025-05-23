use jsonrpc_pubsub::Subscriber;
use log::*;
use magicblock_geyser_plugin::rpc::GeyserRpcService;
use solana_account_decoder::UiAccountEncoding;
use solana_sdk::pubkey::Pubkey;
use tokio_util::sync::CancellationToken;

use crate::{
    errors::reject_internal_error,
    handler::common::handle_account_geyser_update,
    notification_builder::AccountNotificationBuilder,
    subscription::assign_sub_id, types::AccountParams,
};

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

    let mut geyser_rx = geyser_service.accounts_subscribe(subid, pubkey);

    let Some(sink) = assign_sub_id(subscriber, subid) else {
        return;
    };
    let bulder = AccountNotificationBuilder {
        encoding: params.encoding().unwrap_or(UiAccountEncoding::Base58),
    };
    while let Some(val) = geyser_rx.recv() {}
}
