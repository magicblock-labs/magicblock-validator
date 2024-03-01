use std::sync::Arc;

use crossbeam_channel::Receiver;
use sleipnir_stage_verify::metrics::SigverifyTracerPacketStats;
use solana_perf::packet::PacketBatch;

pub(crate) mod packet_batch;
pub(crate) mod packet_deserializer;

pub type BankingPacketBatch = Arc<(Vec<PacketBatch>, Option<SigverifyTracerPacketStats>)>;
