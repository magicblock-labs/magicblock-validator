use std::{str::FromStr, time::Duration};

use geyser_grpc_proto::{geyser, tonic::Status};
use jsonrpc_pubsub::{Sink, Subscriber};
use log::*;
use magicblock_bank::bank::Bank;
use magicblock_geyser_plugin::rpc::GeyserRpcService;
use solana_rpc_client_api::response::{
    ProcessedSignatureResult, RpcSignatureResult,
};
use solana_sdk::{signature::Signature, transaction::TransactionError};
use tokio_util::sync::CancellationToken;

use crate::{
    conversions::{geyser_sub_for_transaction_signature, slot_from_update},
    errors::{reject_internal_error, sink_notify_error},
    subscription::assign_sub_id,
    types::{ResponseWithSubscriptionId, SignatureParams},
};

pub async fn handle_signature_subscribe(
    subid: u64,
    subscriber: Subscriber,
    unsubscriber: CancellationToken,
    params: &SignatureParams,
    geyser_service: &GeyserRpcService,
    bank: &Bank,
) {
    let sigstr = params.signature();
    let sub = geyser_sub_for_transaction_signature(sigstr.to_string());

    let sig = match Signature::from_str(sigstr) {
        Ok(sig) => sig,
        Err(err) => {
            reject_internal_error(subscriber, "Invalid Signature", Some(err));
            return;
        }
    };

    let mut geyser_rx = match geyser_service.transaction_subscribe(
        sub,
        subid,
        unsubscriber,
        Some(&sig),
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
        if let Some((slot, res)) = bank.get_recent_signature_status(
            &sig,
            Some(bank.slots_for_duration(Duration::from_secs(10))),
        ) {
            debug!(
                "Sending initial signature status from bank: {} {:?}",
                slot, res
            );
            sink_notify_transaction_result(&sink, slot, subid, res.err());
        } else {
            tokio::select! {
                val = geyser_rx.recv() => {
                    match val {
                        Some(update) => {
                            if handle_signature_geyser_update(
                                &sink,
                                subid,
                                update) {
                            }
                        }
                        None => {
                            debug!(
                                "Geyser subscription has ended, finishing."
                            );
                        }
                    }
                }
            }
        }
    }
}

/// Handles geyser update for signature subscription.
/// Returns true if subscription has ended.
fn handle_signature_geyser_update(
    sink: &Sink,
    subid: u64,
    update: Result<geyser::SubscribeUpdate, Status>,
) -> bool {
    match update {
        Ok(update) => {
            debug!("Received geyser update: {:?}", update);
            let slot = slot_from_update(&update).unwrap_or(0);
            sink_notify_transaction_result(sink, slot, subid, None);
            // single notification subscription
            // see: https://solana.com/docs/rpc/websocket/signaturesubscribe
            true
        }
        Err(status) => sink_notify_error(
            sink,
            format!("Failed to receive signature update: {:?}", status),
        ),
    }
}

/// Tries to notify the sink about the transaction result.
/// Returns true if the subscription has ended.
fn sink_notify_transaction_result(
    sink: &Sink,
    slot: u64,
    sub_id: u64,
    err: Option<TransactionError>,
) {
    let res = ResponseWithSubscriptionId::new(
        RpcSignatureResult::ProcessedSignature(ProcessedSignatureResult {
            err,
        }),
        slot,
        sub_id,
    );
    if let Err(err) = sink.notify(res.into_params_map()) {
        debug!("Subscription has ended {:?}.", err);
    }
}
