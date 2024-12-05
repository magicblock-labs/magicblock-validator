use geyser_grpc_proto::{geyser, tonic::Status};
use jsonrpc_pubsub::{Sink, Subscriber};
use log::*;
use magicblock_geyser_plugin::rpc::GeyserRpcService;
use tokio_util::sync::CancellationToken;

use crate::{
    conversions::{
        geyser_sub_for_slot_update, subscribe_update_into_slot_response,
    },
    errors::{reject_internal_error, sink_notify_error},
    subscription::assign_sub_id,
    types::ReponseNoContextWithSubscriptionId,
};

pub async fn handle_slot_subscribe(
    subid: u64,
    subscriber: Subscriber,
    unsubscriber: CancellationToken,
    geyser_service: &GeyserRpcService,
) {
    let sub = geyser_sub_for_slot_update();

    let mut geyser_rx =
        match geyser_service.slot_subscribe(sub, subid, unsubscriber) {
            Ok(res) => res,
            Err(err) => {
                reject_internal_error(
                    subscriber,
                    "Failed to subscribe to slot",
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
                                update) {
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
/// Handles geyser update for slot subscription.
/// Returns true if subscription has ended.
fn handle_account_geyser_update(
    sink: &Sink,
    subid: u64,
    update: Result<geyser::SubscribeUpdate, Status>,
) -> bool {
    match update {
        Ok(update) => {
            let slot_response =
                match subscribe_update_into_slot_response(update) {
                    Some(slot_response) => slot_response,
                    None => {
                        debug!("No slot in update, skipping.");
                        return false;
                    }
                };
            let res =
                ReponseNoContextWithSubscriptionId::new(slot_response, subid);
            trace!("Sending Slot update response: {:?}", res);
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
