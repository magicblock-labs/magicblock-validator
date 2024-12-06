use std::sync::Arc;

use crossbeam_channel::Receiver;
use sigverify_packet_stats::SigverifyTracerPacketStats;
use solana_perf::packet::PacketBatch;

pub mod packet_batch;
pub mod packet_deserializer;
pub mod sigverify_packet_stats;

pub type BankingPacketBatch =
    Arc<(Vec<PacketBatch>, Option<SigverifyTracerPacketStats>)>;
pub type BankingPacketReceiver = Receiver<BankingPacketBatch>;
