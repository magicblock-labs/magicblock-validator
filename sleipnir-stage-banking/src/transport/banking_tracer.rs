// NOTE: from core/src/banking_trace.rs :175
use std::sync::Arc;

use crossbeam_channel::{unbounded, Receiver};

use crate::packet::BankingPacketBatch;

use super::{
    traced_sender::{ActiveTracer, ChannelLabel, TracedSender},
    BankingPacketReceiver, BankingPacketSender,
};

pub struct BankingTracer {
    active_tracer: Option<ActiveTracer>,
}

impl BankingTracer {
    pub fn new_disabled() -> Arc<Self> {
        Arc::new(Self {
            active_tracer: None,
        })
    }

    fn create_channel(&self, label: ChannelLabel) -> (BankingPacketSender, BankingPacketReceiver) {
        Self::channel(label, self.active_tracer.as_ref().cloned())
    }

    pub fn create_channel_non_vote(&self) -> (BankingPacketSender, BankingPacketReceiver) {
        self.create_channel(ChannelLabel::NonVote)
    }

    fn channel(
        label: ChannelLabel,
        active_tracer: Option<ActiveTracer>,
    ) -> (TracedSender, Receiver<BankingPacketBatch>) {
        let (sender, receiver) = unbounded();
        (TracedSender::new(label, sender, active_tracer), receiver)
    }
}
