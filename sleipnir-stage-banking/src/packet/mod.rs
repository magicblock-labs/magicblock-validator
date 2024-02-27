use std::sync::Arc;

use crossbeam_channel::Receiver;
use sleipnir_stage_verify::metrics::SigverifyTracerPacketStats;
use solana_perf::packet::PacketBatch;

use self::traced_sender::TracedSender;

pub(crate) mod packet_deserializer;
mod traced_sender;

pub type BankingPacketBatch = Arc<(Vec<PacketBatch>, Option<SigverifyTracerPacketStats>)>;
pub type BankingPacketSender = TracedSender;
pub type BankingPacketReceiver = Receiver<BankingPacketBatch>;
