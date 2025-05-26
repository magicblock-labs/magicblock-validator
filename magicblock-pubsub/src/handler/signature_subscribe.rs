use std::{str::FromStr, time::Duration};

use jsonrpc_pubsub::{Sink, Subscriber};
use log::debug;
use magicblock_bank::bank::Bank;
use magicblock_geyser_plugin::rpc::GeyserRpcService;
use solana_rpc_client_api::response::{
    ProcessedSignatureResult, RpcSignatureResult,
};
use solana_sdk::{signature::Signature, transaction::TransactionError};

use crate::{
    errors::reject_internal_error,
    notification_builder::SignatureNotificationBulider,
    subscription::assign_sub_id,
    types::{ResponseWithSubscriptionId, SignatureParams},
};

use super::common::UpdateHandler;

pub async fn handle_signature_subscribe(
    subid: u64,
    subscriber: Subscriber,
    params: &SignatureParams,
    geyser_service: &GeyserRpcService,
    bank: &Bank,
) {
    let sig = match Signature::from_str(params.signature()) {
        Ok(sig) => sig,
        Err(err) => {
            reject_internal_error(subscriber, "Invalid Signature", Some(err));
            return;
        }
    };

    let mut geyser_rx = geyser_service.transaction_subscribe(subid, sig);
    let subscriptions_db = geyser_service.subscriptions_db.clone();
    let Some(sink) = assign_sub_id(subscriber, subid) else {
        return;
    };
    if let Some((slot, res)) = bank.get_recent_signature_status(
        &sig,
        Some(bank.slots_for_duration(Duration::from_secs(10))),
    ) {
        debug!(
            "Sending initial signature status from bank: {} {:?}",
            slot, res
        );
        sink_notify_transaction_result(&sink, slot, subid, res.err());
        subscriptions_db.unsubscribe_from_signature(&sig, subid);
        return;
    }
    let builder = SignatureNotificationBulider {};
    let cleanup = move || {
        subscriptions_db.unsubscribe_from_signature(&sig, subid);
    };
    let handler = UpdateHandler::new_with_sink(sink, subid, builder, cleanup);
    // Note: 60 seconds should be more than enough for any transaction confirmation,
    // if it wasn't confirmed during this period, then it was never executed, thus we
    // can just cancel the subscription to free up resources
    let rx = tokio::time::timeout(Duration::from_secs(60), geyser_rx.recv());
    let Ok(Some(msg)) = rx.await else {
        return;
    };
    handler.handle(msg);
}

/// Handles geyser update for signature subscription.
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
