use std::time::Instant;

use log::*;
use tokio_util::sync::CancellationToken;

use crate::{
    handler::account_subscribe::handle_account_subscribe,
    subscription::SubscriptionRequest,
};

mod account_subscribe;

pub async fn handle_subscription(
    subscription: SubscriptionRequest,
    subid: u64,
    unsubscriber: CancellationToken,
) {
    match subscription {
        SubscriptionRequest::AccountSubscribe {
            subscriber,
            geyser_service,
            params,
        } => {
            let start = Instant::now();
            loop {
                tokio::select! {
                    _ = unsubscriber.cancelled() => {
                        debug!("AccountUnsubscribe: {}", subid);
                        break;
                    },
                    _ = handle_account_subscribe(
                            subid,
                            subscriber,
                            &params,
                            &geyser_service) => {
                        break;
                    },
                };
            }
            let elapsed = start.elapsed();
            debug!("accountSubscribe {} lasted for {:?}", subid, elapsed);
        }
    }
}
