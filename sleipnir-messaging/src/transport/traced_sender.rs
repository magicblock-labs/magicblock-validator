use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::SystemTime,
};

use crossbeam_channel::{SendError, Sender};
use log::error;
use serde_derive::{Deserialize, Serialize};
use solana_frozen_abi_macro::{frozen_abi, AbiEnumVisitor, AbiExample};
use solana_sdk::{hash::Hash, slot_history::Slot};

use crate::packet::BankingPacketBatch;

#[derive(Serialize, Deserialize, Debug, AbiExample, AbiEnumVisitor)]
pub enum TracedEvent {
    PacketBatch(ChannelLabel, BankingPacketBatch),
    BlockAndBankHash(Slot, Hash, Hash),
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, AbiExample, AbiEnumVisitor)]
pub enum ChannelLabel {
    NonVote,
    TpuVote,
    GossipVote,
    Dummy,
}

#[frozen_abi(digest = "Eq6YrAFtTbtPrCEvh6Et1mZZDCARUg1gcK2qiZdqyjUz")]
#[derive(Serialize, Deserialize, Debug, AbiExample)]
pub struct TimedTracedEvent(pub std::time::SystemTime, pub TracedEvent);

#[derive(Clone, Debug)]
pub(crate) struct ActiveTracer {
    trace_sender: Sender<TimedTracedEvent>,
    exit: Arc<AtomicBool>,
}

pub struct TracedSender {
    label: ChannelLabel,
    sender: Sender<BankingPacketBatch>,
    active_tracer: Option<ActiveTracer>,
}

impl TracedSender {
    pub(crate) fn new(
        label: ChannelLabel,
        sender: Sender<BankingPacketBatch>,
        active_tracer: Option<ActiveTracer>,
    ) -> Self {
        Self {
            label,
            sender,
            active_tracer,
        }
    }

    pub fn send(&self, batch: BankingPacketBatch) -> Result<(), SendError<BankingPacketBatch>> {
        if let Some(ActiveTracer { trace_sender, exit }) = &self.active_tracer {
            if !exit.load(Ordering::Relaxed) {
                trace_sender
                    .send(TimedTracedEvent(
                        SystemTime::now(),
                        TracedEvent::PacketBatch(self.label, BankingPacketBatch::clone(&batch)),
                    ))
                    .map_err(|err| {
                        error!(
                            "unexpected error when tracing a banking event...: {:?}",
                            err
                        );
                        SendError(BankingPacketBatch::clone(&batch))
                    })?;
            }
        }
        self.sender.send(batch)
    }
}
