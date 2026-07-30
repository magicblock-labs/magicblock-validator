use serde::{Deserialize, Serialize};

use crate::types::SerdeKeypair;

/// Configuration for the internal task scheduler.
///
/// Task execution is performed by the external hydra cranker service, so the
/// validator only needs the keypair used to pay for (sponsor, fund, and cancel)
/// hydra cranks. This faucet keypair is delegated on startup; it must be funded
/// separately (the validator does not fund it).
#[derive(Deserialize, Default, Serialize, Debug, Clone)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct TaskSchedulerConfig {
    /// Keypair the task scheduler uses to pay for hydra cranks, encoded in
    /// Base58.
    pub faucet_keypair: Option<SerdeKeypair>,
}
