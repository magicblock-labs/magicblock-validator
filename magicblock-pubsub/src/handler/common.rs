use jsonrpc_pubsub::{Sink, Subscriber};
use log::debug;
use magicblock_geyser_plugin::types::GeyserMessage;
use serde::{Deserialize, Serialize};
use solana_account_decoder::UiAccount;

use crate::{
    notification_builder::NotificationBuilder, subscription::assign_sub_id,
    types::ResponseWithSubscriptionId,
};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct UiAccountWithPubkey {
    pub pubkey: String,
    pub account: UiAccount,
}

pub struct UpdateHandler<B, C: FnOnce()> {
    pub sink: Sink,
    pub subid: u64,
    pub builder: B,
    pub cleanup: Option<C>,
}

impl<B: NotificationBuilder, C: FnOnce()> UpdateHandler<B, C> {
    pub fn new(
        subid: u64,
        subscriber: Subscriber,
        builder: B,
        cleanup: C,
    ) -> Option<Self> {
        let sink = assign_sub_id(subscriber, subid)?;
        Some(Self::new_with_sink(sink, subid, builder, cleanup))
    }

    pub fn new_with_sink(
        sink: Sink,
        subid: u64,
        builder: B,
        cleanup: C,
    ) -> Self {
        Self {
            sink,
            subid,
            builder,
            cleanup: Some(cleanup),
        }
    }

    pub fn handle(&self, msg: GeyserMessage) -> bool {
        let Some((update, slot)) = self.builder.try_build_notifcation(msg)
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
}

impl<B, C: FnOnce()> Drop for UpdateHandler<B, C> {
    fn drop(&mut self) {
        if let Some(callback) = self.cleanup.take() {
            (callback)()
        }
    }
}
