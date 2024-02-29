use self::traced_sender::TracedSender;
pub(crate) mod banking_tracer;
pub(crate) mod traced_sender;

pub type BankingPacketSender = TracedSender;
pub type BankingPacketReceiver = Receiver<BankingPacketBatch>;
