use std::time::Instant;

use log::*;
use tokio_util::sync::CancellationToken;

use crate::{
    handler::{
        account_subscribe::handle_account_subscribe,
        signature_subscribe::handle_signature_subscribe,
        slot_subscribe::handle_slot_subscribe,
    },
    subscription::SubscriptionRequest,
};

mod account_subscribe;
mod signature_subscribe;
mod slot_subscribe;

pub async fn handle_subscription(
    subscription: SubscriptionRequest,
    subid: u64,
    unsubscriber: CancellationToken,
) {
    use SubscriptionRequest::*;
    match subscription {
        Account {
            subscriber,
            geyser_service,
            params,
        } => {
            let start = Instant::now();
            tokio::select! {
                _ = unsubscriber.cancelled() => {
                    debug!("AccountUnsubscribe: {}", subid);
                },
                _ = handle_account_subscribe(
                        subid,
                        subscriber,
                        &params,
                        &geyser_service) => {
                },
            };
            let elapsed = start.elapsed();
            debug!("accountSubscribe {} lasted for {:?}", subid, elapsed);
        }
        Slot {
            subscriber,
            geyser_service,
        } => {
            let start = Instant::now();
            tokio::select! {
                _ = unsubscriber.cancelled() => {
                    debug!("SlotUnsubscribe: {}", subid);
                },
                _ = handle_slot_subscribe(
                        subid,
                        subscriber,
                        &geyser_service) => {
                },
            };
            let elapsed = start.elapsed();
            debug!("slotSubscribe {} lasted for {:?}", subid, elapsed);
        }

        Signature {
            subscriber,
            geyser_service,
            params,
            bank,
        } => {
            let start = Instant::now();
            tokio::select! {
                _ = unsubscriber.cancelled() => {
                    debug!("SignatureUnsubscribe: {}", subid);
                },
                _ = handle_signature_subscribe(
                        subid,
                        subscriber,
                        &params,
                        &geyser_service,
                        &bank) => {
                },
            };
            let elapsed = start.elapsed();
            debug!("slotSubscribe {} lasted for {:?}", subid, elapsed);
        }
    }
}
