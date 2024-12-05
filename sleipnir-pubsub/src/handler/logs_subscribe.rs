use geyser_grpc_proto::{geyser, tonic::Status};
use jsonrpc_pubsub::{Sink, Subscriber};
use log::*;
use magicblock_geyser_plugin::rpc::GeyserRpcService;
use tokio_util::sync::CancellationToken;

use crate::{
    conversions::{
        slot_from_update, try_geyser_sub_for_transaction_logs,
        try_subscribe_update_into_logs,
    },
    errors::{reject_internal_error, sink_notify_error},
    subscription::assign_sub_id,
    types::{LogsParams, ResponseWithSubscriptionId},
};

pub async fn handle_logs_subscribe(
    subid: u64,
    subscriber: Subscriber,
    unsubscriber: CancellationToken,
    params: &LogsParams,
    geyser_service: &GeyserRpcService,
) {
    let filter = params.filter();
    // NOTE: the config only includes the commitment level which we don't use
    let _config = params.config();

    let sub = match try_geyser_sub_for_transaction_logs(filter) {
        Ok(sub) => sub,
        Err(err) => {
            reject_internal_error(subscriber, "Invalid filter", Some(err));
            return;
        }
    };

    let mut geyser_rx = match geyser_service.transaction_subscribe(
        sub,
        subid,
        unsubscriber,
        None,
    ) {
        Ok(res) => res,
        Err(err) => {
            reject_internal_error(
                subscriber,
                "Failed to subscribe to logs",
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
                            if handle_transaction_logs_geyser_update(
                                &sink,
                                subid,
                                update,
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

/// Handles geyser update for transaction logs subscription.
/// Returns true if subscription has ended.
fn handle_transaction_logs_geyser_update(
    sink: &Sink,
    subid: u64,
    update: Result<geyser::SubscribeUpdate, Status>,
) -> bool {
    match update {
        Ok(update) => {
            debug!("Received geyser update: {:?}", update);
            let slot = slot_from_update(&update).unwrap_or(0);
            match try_subscribe_update_into_logs(update) {
                Ok(Some(logs)) => {
                    let res =
                        ResponseWithSubscriptionId::new(logs, slot, subid);
                    debug!("Sending response: {:?}", res);
                    if let Err(err) = sink.notify(res.into_params_map()) {
                        debug!("Subscription has ended, finishing {:?}.", err);
                        true
                    } else {
                        false
                    }
                }
                Ok(None) => {
                    warn!("No complete logs found in update, skipping.");
                    false
                }
                Err(err) => {
                    let msg =
                        format!("Failed to convert update to logs: {:?}", err);
                    sink_notify_error(sink, msg)
                }
            }
        }
        Err(status) => sink_notify_error(
            sink,
            format!("Failed to receive transaction logs update: {:?}", status),
        ),
    }
}
