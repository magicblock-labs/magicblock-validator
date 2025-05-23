use jsonrpc_pubsub::Subscriber;
use log::*;
use magicblock_geyser_plugin::rpc::GeyserRpcService;
use solana_sdk::pubkey::Pubkey;
use tokio_util::sync::CancellationToken;

use crate::{
    conversions::geyser_sub_for_account, errors::reject_internal_error,
    handler::common::handle_account_geyser_update, subscription::assign_sub_id,
    types::AccountParams,
};

pub async fn handle_account_subscribe(
    subid: u64,
    subscriber: Subscriber,
    unsubscriber: CancellationToken,
    params: &AccountParams,
    geyser_service: &GeyserRpcService,
) {
    let pubkey = match Pubkey::try_from(address) {
        Ok(pubkey) => pubkey,
        Err(err) => {
            reject_internal_error(subscriber, "Invalid Pubkey", Some(err));
            return;
        }
    };

    let mut geyser_rx = match geyser_service.accounts_subscribe(
        sub,
        subid,
        unsubscriber,
        Some(&pubkey),
    ) {
        Ok(res) => res,
        Err(err) => {
            reject_internal_error(
                subscriber,
                "Failed to subscribe to signature",
                Some(err),
            );
            return;
        }
    };

    if let Some(sink) = assign_sub_id(subscriber, subid) {
        loop {
            tokio::select! {
                val = geyser_rx.recv() => {
                    match val {
                        Some(update) => {
                            if handle_account_geyser_update(
                                &sink,
                                subid,
                                update,
                                params.into(),
                                false,
                            ) {
                                break;
                            }
                        }
                        None => {
                            debug!(
                                "Geyser subscription has ended, finishing."
                            );
                            break;
                        }
                    }
                }
            }
        }
    }
}
