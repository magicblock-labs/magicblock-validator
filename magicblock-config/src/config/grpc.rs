use serde::{Deserialize, Serialize};

/// Global configuration for gRPC-based providers (e.g., Helius Laser).
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct GrpcConfig {
    /// The maximum number of subscriptions that are added to a single optimized stream
    pub max_subs_in_old_optimized: usize,
    /// The maximum number of old unoptimized subscriptions streams allowed until optimization is triggered
    pub max_old_unoptimized: usize,
    /// The maximum number of subscriptions held in the current new stream which is updated frequently
    pub max_subs_in_new: usize,
}

impl Default for GrpcConfig {
    fn default() -> Self {
        Self {
            max_subs_in_old_optimized: 5000,
            max_old_unoptimized: 5,
            max_subs_in_new: 400,
        }
    }
}
