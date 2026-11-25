/// Describes how commit data is delivered to the base layer.
///
/// Small accounts send data directly in instruction args.
/// Large accounts use an on-chain buffer to avoid transaction size limits.
/// When a base account is available, a diff is computed to reduce payload size.
#[derive(Clone, Debug)]
pub enum CommitDelivery {
    StateInArgs,
    StateInBuffer,
    DiffInArgs { diff: Vec<u8> },
    DiffInBuffer { diff: Vec<u8> },
}
