use crossbeam_channel::Receiver;

use crate::packet::BankingPacketBatch;

use self::traced_sender::TracedSender;
pub mod banking_tracer;
pub mod traced_sender;

pub type BankingPacketSender = TracedSender;
pub type BankingPacketReceiver = Receiver<BankingPacketBatch>;
