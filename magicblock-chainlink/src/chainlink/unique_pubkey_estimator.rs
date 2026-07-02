use std::{
    collections::{hash_map::DefaultHasher, HashMap},
    hash::{Hash, Hasher},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use magicblock_metrics::metrics::{
    self, ChainlinkUniquePubkeyWindow, LabelValue,
};
use solana_pubkey::Pubkey;

const HLL_PRECISION: u8 = 12;
const HLL_REGISTER_COUNT: usize = 1 << HLL_PRECISION;
const BUCKET_SECONDS: u64 = 60;
const BUCKET_COUNT: usize = 60;
const EXPORT_INTERVAL_SECONDS: u64 = 15;
const WINDOWS: [ChainlinkUniquePubkeyWindow; 3] = [
    ChainlinkUniquePubkeyWindow::OneMinute,
    ChainlinkUniquePubkeyWindow::FiveMinutes,
    ChainlinkUniquePubkeyWindow::OneHour,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UniquePubkeyStage {
    RequestedByRpc,
    BankMiss,
    RemoteFetch,
    RemoteNotFound,
    SubscriptionAlreadyPresent,
    SubscriptionAdded,
    SubscriptionEvicted,
    CompanionFetch,
}

impl LabelValue for UniquePubkeyStage {
    fn value(&self) -> &str {
        match self {
            Self::RequestedByRpc => "requested_by_rpc",
            Self::BankMiss => "bank_miss",
            Self::RemoteFetch => "remote_fetch",
            Self::RemoteNotFound => "remote_not_found",
            Self::SubscriptionAlreadyPresent => "subscription_already_present",
            Self::SubscriptionAdded => "subscription_added",
            Self::SubscriptionEvicted => "subscription_evicted",
            Self::CompanionFetch => "companion_fetch",
        }
    }
}

const UNIQUE_PUBKEY_STAGES: [UniquePubkeyStage; 8] = [
    UniquePubkeyStage::RequestedByRpc,
    UniquePubkeyStage::BankMiss,
    UniquePubkeyStage::RemoteFetch,
    UniquePubkeyStage::RemoteNotFound,
    UniquePubkeyStage::SubscriptionAlreadyPresent,
    UniquePubkeyStage::SubscriptionAdded,
    UniquePubkeyStage::SubscriptionEvicted,
    UniquePubkeyStage::CompanionFetch,
];

#[derive(Debug)]
pub(crate) struct UniquePubkeyEstimator {
    series: Mutex<HashMap<String, HashMap<String, UniquePubkeySeries>>>,
    last_export_epoch_seconds: AtomicU64,
}

impl Default for UniquePubkeyEstimator {
    fn default() -> Self {
        debug_assert_unique_stage_labels();
        Self {
            series: Mutex::default(),
            last_export_epoch_seconds: AtomicU64::default(),
        }
    }
}

impl UniquePubkeyEstimator {
    pub(crate) fn observe(
        &self,
        origin: impl LabelValue,
        stage: impl LabelValue,
        pubkey: &Pubkey,
    ) {
        self.observe_at(origin, stage, pubkey, now_epoch_seconds());
    }

    pub(crate) fn observe_many<'a>(
        &self,
        origin: impl LabelValue + Copy,
        stage: impl LabelValue + Copy,
        pubkeys: impl IntoIterator<Item = &'a Pubkey>,
    ) {
        let now_epoch_seconds = now_epoch_seconds();
        for pubkey in pubkeys {
            self.observe_at(origin, stage, pubkey, now_epoch_seconds);
        }
    }

    fn observe_at(
        &self,
        origin: impl LabelValue,
        stage: impl LabelValue,
        pubkey: &Pubkey,
        now_epoch_seconds: u64,
    ) {
        let pubkey_hash = hash_pubkey(pubkey);
        let epoch_minute = now_epoch_seconds / BUCKET_SECONDS;
        let origin_label = origin.value();
        let stage_label = stage.value();
        let should_export = self.should_export(now_epoch_seconds);

        let mut series =
            self.series.lock().unwrap_or_else(|err| err.into_inner());
        if !series.contains_key(origin_label) {
            series.insert(origin_label.to_string(), HashMap::new());
        }
        let stage_series = series
            .get_mut(origin_label)
            .expect("origin series was just inserted");
        if !stage_series.contains_key(stage_label) {
            stage_series.insert(
                stage_label.to_string(),
                UniquePubkeySeries::new(origin_label, stage_label),
            );
        }
        let pubkey_series = stage_series
            .get_mut(stage_label)
            .expect("stage series was just inserted");
        pubkey_series.observe_hash(epoch_minute, pubkey_hash);

        if should_export {
            export_series(&series, epoch_minute);
        }
    }

    fn should_export(&self, now_epoch_seconds: u64) -> bool {
        let last_export_epoch_seconds =
            self.last_export_epoch_seconds.load(Ordering::Relaxed);
        if now_epoch_seconds.saturating_sub(last_export_epoch_seconds)
            < EXPORT_INTERVAL_SECONDS
        {
            return false;
        }
        self.last_export_epoch_seconds
            .compare_exchange(
                last_export_epoch_seconds,
                now_epoch_seconds,
                Ordering::AcqRel,
                Ordering::Relaxed,
            )
            .is_ok()
    }

    #[cfg(any(test, feature = "dev-context"))]
    #[cfg_attr(feature = "dev-context", allow(dead_code))]
    pub fn force_export_for_tests(&self, now_epoch_seconds: u64) {
        let epoch_minute = now_epoch_seconds / BUCKET_SECONDS;
        let series = self.series.lock().unwrap_or_else(|err| err.into_inner());
        export_series(&series, epoch_minute);
    }
}

#[derive(Debug)]
struct UniquePubkeySeries {
    origin_label: String,
    stage_label: String,
    buckets: Vec<MinuteBucket>,
}

impl UniquePubkeySeries {
    fn new(origin_label: &str, stage_label: &str) -> Self {
        let buckets =
            (0..BUCKET_COUNT).map(|_| MinuteBucket::default()).collect();
        Self {
            origin_label: origin_label.to_string(),
            stage_label: stage_label.to_string(),
            buckets,
        }
    }

    fn observe_hash(&mut self, epoch_minute: u64, pubkey_hash: u64) {
        let bucket_index = (epoch_minute as usize) % BUCKET_COUNT;
        let bucket = &mut self.buckets[bucket_index];
        if bucket.epoch_minute != epoch_minute {
            bucket.epoch_minute = epoch_minute;
            bucket.sketch.clear();
        }
        bucket.sketch.insert_hash(pubkey_hash);
    }

    fn estimate_window(
        &self,
        now_epoch_minute: u64,
        window_minutes: u64,
    ) -> f64 {
        let mut merged = HllSketch::default();
        for bucket in &self.buckets {
            if now_epoch_minute.saturating_sub(bucket.epoch_minute)
                < window_minutes
            {
                merged.merge(&bucket.sketch);
            }
        }
        merged.estimate()
    }
}

#[derive(Debug, Default)]
struct MinuteBucket {
    epoch_minute: u64,
    sketch: HllSketch,
}

#[derive(Debug, Clone)]
struct HllSketch {
    registers: [u8; HLL_REGISTER_COUNT],
}

impl Default for HllSketch {
    fn default() -> Self {
        Self {
            registers: [0; HLL_REGISTER_COUNT],
        }
    }
}

impl HllSketch {
    fn clear(&mut self) {
        self.registers = [0; HLL_REGISTER_COUNT];
    }

    fn insert_hash(&mut self, hash: u64) {
        let index_mask = HLL_REGISTER_COUNT as u64 - 1;
        let register_index = (hash & index_mask) as usize;
        let remainder = hash >> HLL_PRECISION;
        let remaining_bits = u64::BITS - HLL_PRECISION as u32;
        let rank = if remainder == 0 {
            remaining_bits
        } else {
            (remainder.leading_zeros() - HLL_PRECISION as u32 + 1)
                .min(remaining_bits)
        } as u8;
        self.registers[register_index] =
            self.registers[register_index].max(rank);
    }

    fn merge(&mut self, other: &Self) {
        for (register, other_register) in
            self.registers.iter_mut().zip(other.registers.iter())
        {
            *register = (*register).max(*other_register);
        }
    }

    fn estimate(&self) -> f64 {
        let m = HLL_REGISTER_COUNT as f64;
        let alpha = 0.7213 / (1.0 + 1.079 / m);
        let mut harmonic_sum = 0.0;
        let mut zero_registers = 0_u64;
        for register in self.registers {
            harmonic_sum += 2_f64.powi(-(register as i32));
            if register == 0 {
                zero_registers += 1;
            }
        }
        let raw_estimate = alpha * m * m / harmonic_sum;
        if raw_estimate <= 2.5 * m && zero_registers > 0 {
            m * (m / zero_registers as f64).ln()
        } else {
            raw_estimate
        }
    }
}

fn export_series(
    series: &HashMap<String, HashMap<String, UniquePubkeySeries>>,
    now_epoch_minute: u64,
) {
    for stage_series in series.values() {
        for pubkey_series in stage_series.values() {
            for window in WINDOWS {
                let estimate = pubkey_series
                    .estimate_window(now_epoch_minute, window.minutes())
                    .round() as u64;
                metrics::set_chainlink_unique_pubkeys_estimate(
                    &pubkey_series.origin_label,
                    &pubkey_series.stage_label,
                    window,
                    estimate,
                );
            }
        }
    }
}

fn debug_assert_unique_stage_labels() {
    for (index, stage) in UNIQUE_PUBKEY_STAGES.iter().enumerate() {
        for other_stage in UNIQUE_PUBKEY_STAGES.iter().skip(index + 1) {
            debug_assert_ne!(stage.value(), other_stage.value());
        }
    }
}

fn hash_pubkey(pubkey: &Pubkey) -> u64 {
    let mut hasher = DefaultHasher::new();
    pubkey.hash(&mut hasher);
    hasher.finish()
}

fn now_epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use magicblock_metrics::metrics::{
        chainlink_unique_pubkeys_estimate_value, AccountFetchOrigin,
    };
    use solana_signature::Signature;

    use super::*;

    fn pubkey_from_u64(value: u64) -> Pubkey {
        let mut bytes = [0; 32];
        bytes[..8].copy_from_slice(&value.to_le_bytes());
        Pubkey::new_from_array(bytes)
    }

    #[test]
    fn hll_estimate_for_known_set_is_within_reasonable_error() {
        let mut sketch = HllSketch::default();
        for value in 0..10_000 {
            sketch.insert_hash(hash_pubkey(&pubkey_from_u64(value)));
        }

        let estimate = sketch.estimate();
        let error_ratio = ((estimate - 10_000.0) / 10_000.0).abs();
        assert!(
            error_ratio <= 0.10,
            "estimate {estimate} should be within 10% of 10,000"
        );
    }

    #[test]
    fn bucket_rotation_drops_old_window_entries() {
        let estimator = UniquePubkeyEstimator::default();
        let origin = AccountFetchOrigin::GetAccount;
        let stage = UniquePubkeyStage::RequestedByRpc;
        estimator.observe_at(origin, stage, &pubkey_from_u64(1), 0);
        estimator.observe_at(origin, stage, &pubkey_from_u64(2), 120);
        estimator.force_export_for_tests(120);

        assert_eq!(
            chainlink_unique_pubkeys_estimate_value(
                &origin,
                &stage,
                ChainlinkUniquePubkeyWindow::OneMinute,
            ),
            1
        );
        assert_eq!(
            chainlink_unique_pubkeys_estimate_value(
                &origin,
                &stage,
                ChainlinkUniquePubkeyWindow::FiveMinutes,
            ),
            2
        );
    }

    #[test]
    fn send_transaction_origin_does_not_include_signature() {
        let estimator = UniquePubkeyEstimator::default();
        let stage = UniquePubkeyStage::RemoteFetch;
        estimator.observe_at(
            AccountFetchOrigin::SendTransaction(Signature::new_unique()),
            stage,
            &pubkey_from_u64(10),
            0,
        );
        estimator.observe_at(
            AccountFetchOrigin::SendTransaction(Signature::new_unique()),
            stage,
            &pubkey_from_u64(11),
            0,
        );
        estimator.force_export_for_tests(0);

        assert_eq!(
            chainlink_unique_pubkeys_estimate_value(
                &AccountFetchOrigin::SendTransaction(Signature::new_unique()),
                &stage,
                ChainlinkUniquePubkeyWindow::OneMinute,
            ),
            2
        );
    }
}
