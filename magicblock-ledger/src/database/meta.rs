use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct AddressSignatureMeta {
    pub writeable: bool,
}

/// Version of the [`PerfSample`] introduced in 1.15.x.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct PerfSample {
    pub num_transactions: u64,
    pub num_slots: u64,
    pub sample_period_secs: u16,
    pub num_non_vote_transactions: u64,
}
