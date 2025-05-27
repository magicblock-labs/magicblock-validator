use std::future::Future;

use jsonrpc_pubsub::{Sink, Subscriber};
use log::debug;
use magicblock_geyser_plugin::types::GeyserMessage;
use serde::{Deserialize, Serialize};
use solana_account_decoder::UiAccount;

use crate::{
    notification_builder::NotificationBuilder,
    subscription::assign_sub_id,
    types::{ResponseNoContextWithSubscriptionId, ResponseWithSubscriptionId},
};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct UiAccountWithPubkey {
    pub pubkey: String,
    pub account: UiAccount,
}

pub struct UpdateHandler<B, C: Future<Output = ()> + Send + Sync + 'static> {
    sink: Sink,
    subid: u64,
    builder: B,
    _cleanup: Cleanup<C>,
}

pub struct Cleanup<F: Future<Output = ()> + Send + Sync + 'static>(Option<F>);

impl<F: Future<Output = ()> + Send + Sync + 'static> From<F> for Cleanup<F> {
    fn from(value: F) -> Self {
        Self(Some(value))
    }
}

impl<B, C> UpdateHandler<B, C>
where
    B: NotificationBuilder,
    C: Future<Output = ()> + Send + Sync + 'static,
{
    pub fn new(
        subid: u64,
        subscriber: Subscriber,
        builder: B,
        cleanup: Cleanup<C>,
    ) -> Option<Self> {
        let sink = assign_sub_id(subscriber, subid)?;
        Some(Self::new_with_sink(sink, subid, builder, cleanup))
    }

    pub fn new_with_sink(
        sink: Sink,
        subid: u64,
        builder: B,
        cleanup: Cleanup<C>,
    ) -> Self {
        Self {
            sink,
            subid,
            builder,
            _cleanup: cleanup,
        }
    }

    pub fn handle(&self, msg: GeyserMessage) -> bool {
        let Some((update, slot)) = self.builder.try_build_notification(msg)
        else {
            // NOTE: messages are targetted, so builder will always
            // succeed, this branch just avoids eyesore unwraps
            return true;
        };
        let notification =
            ResponseWithSubscriptionId::new(update, slot, self.subid);
        if let Err(err) = self.sink.notify(notification.into_params_map()) {
            debug!("Subscription {} has ended {:?}.", self.subid, err);
            false
        } else {
            true
        }
    }

    pub fn handle_slot_update(&self, msg: GeyserMessage) -> bool {
        let Some((update, _)) = self.builder.try_build_notification(msg) else {
            // NOTE: messages are targetted, so builder will always
            // succeed, this branch just avoids eyesore unwraps
            return true;
        };
        let notification =
            ResponseNoContextWithSubscriptionId::new(update, self.subid);
        if let Err(err) = self.sink.notify(notification.into_params_map()) {
            debug!("Subscription {} has ended {:?}.", self.subid, err);
            false
        } else {
            true
        }
    }
}

impl<C: Future<Output = ()> + Send + Sync + 'static> Drop for Cleanup<C> {
    fn drop(&mut self) {
        if let Some(cb) = self.0.take() {
            tokio::spawn(cb);
        }
    }
}
