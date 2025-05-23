use geyser_grpc_proto::{geyser, tonic::Status};
use jsonrpc_pubsub::{Sink, Subscriber};
use log::*;
use magicblock_geyser_plugin::{rpc::GeyserRpcService, types::GeyserMessage};

use crate::{
    conversions::subscribe_update_into_slot_response,
    subscription::assign_sub_id, types::ReponseNoContextWithSubscriptionId,
};

pub async fn handle_slot_subscribe(
    subid: u64,
    subscriber: Subscriber,
    geyser_service: &GeyserRpcService,
) {
    let mut geyser_rx = geyser_service.slot_subscribe(subid);

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
    update: GeyserMessage,
) -> bool {
    let slot_response = match subscribe_update_into_slot_response(update) {
        Some(slot_response) => slot_response,
        None => {
            debug!("No slot in update, skipping.");
            return false;
        }
    };
    let res = ReponseNoContextWithSubscriptionId::new(slot_response, subid);
    trace!("Sending Slot update response: {:?}", res);
    if let Err(err) = sink.notify(res.into_params_map()) {
        debug!("Subscription has ended, finishing {:?}.", err);
        true
    } else {
        false
    }
}
