use geyser_grpc_proto::{geyser, tonic::Status};
use jsonrpc_pubsub::{Sink, Subscriber};
use log::*;
use sleipnir_geyser_plugin::rpc::GeyserRpcService;
use sleipnir_rpc_client_api::config::UiAccountEncoding;
use solana_sdk::pubkey::Pubkey;
use tokio_util::sync::CancellationToken;

use crate::{
    conversions::{
        geyser_sub_for_account, slot_from_update,
        subscribe_update_try_into_ui_account,
    },
    errors::{reject_internal_error, sink_notify_error},
    subscription::assign_sub_id,
    types::{AccountParams, ResponseWithSubscriptionId},
};

pub async fn handle_account_subscribe(
    subid: u64,
    subscriber: Subscriber,
    unsubscriber: CancellationToken,
    params: &AccountParams,
    geyser_service: &GeyserRpcService,
) {
    let address = params.pubkey();
    let sub = geyser_sub_for_account(address.to_string());
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
        &pubkey,
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
                                params) {
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

/// Handles geyser update for account subscription.
/// Returns true if subscription has ended.
fn handle_account_geyser_update(
    sink: &Sink,
    subid: u64,
    update: Result<geyser::SubscribeUpdate, Status>,
    params: &AccountParams,
) -> bool {
    match update {
        Ok(update) => {
            debug!("Received geyser update: {:?}", update);

            let slot = slot_from_update(&update).unwrap_or(0);

            let encoding =
                params.encoding().unwrap_or(UiAccountEncoding::Base58);
            let ui_account = match subscribe_update_try_into_ui_account(
                update,
                encoding,
                params.data_slice_config(),
            ) {
                Ok(Some(ui_account)) => ui_account,
                Ok(None) => {
                    debug!("No account data in update, skipping.");
                    return false;
                }
                Err(err) => {
                    let msg = format!(
                        "Failed to convert update to UiAccount: {:?}",
                        err
                    );
                    let failed_to_notify = sink_notify_error(sink, msg);
                    return failed_to_notify;
                }
            };
            let res = ResponseWithSubscriptionId::new(ui_account, slot, subid);
            debug!("Sending response: {:?}", res);

            if let Err(err) = sink.notify(res.into_params_map()) {
                debug!("Subscription has ended, finishing {:?}.", err);
                true
            } else {
                false
            }
        }
        Err(status) => sink_notify_error(
            sink,
            format!("Failed to receive signature update: {:?}", status),
        ),
    }
}
