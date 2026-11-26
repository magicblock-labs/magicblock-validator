//! Slot-production check across successive one-shot healthchecks.

use std::{
    fs::{self, OpenOptions},
    io::ErrorKind,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail, ensure};
use humantime::re::humantime as ht;
use serde::{Deserialize, Serialize};
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use tempfile::NamedTempFile;
use tracing::debug;

use super::deadline::Deadline;

#[derive(clap::Args)]
pub(super) struct SlotProgress {
    /// File for slot samples and strike state; use a separate file per validator.
    #[arg(long, required = false, requires = "expected_slot_time")]
    slot_state: PathBuf,
    /// Validator's configured block time, such as 50ms.
    #[arg(long, required = false, requires = "slot_state", value_parser = parse_slot_time)]
    expected_slot_time: Duration,
}

fn parse_slot_time(value: &str) -> Result<Duration> {
    let duration = ht::parse_duration(value)?;
    ensure!(!duration.is_zero(), "expected slot time must be positive");
    Ok(duration)
}

#[derive(Deserialize, Serialize)]
struct Sample {
    time: Duration,
    slot: u64,
    strike: bool,
}

impl SlotProgress {
    pub(super) async fn check(&self, deadline: Deadline, rpc: &RpcClient) -> Result<()> {
        let path = &self.slot_state;
        let slot_time = self.expected_slot_time;
        // Lock a stable file, not the state file that we replace on each write.
        let mut lock_path = path.as_os_str().to_os_string();
        lock_path.push(".lock");
        let lock = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .with_context(|| format!("opening slot state lock {lock_path:?}"))?;
        lock.try_lock()
            .with_context(|| format!("locking slot state {}", path.display()))?;

        let previous: Option<Sample> = match fs::read(path) {
            Ok(data) => Some(
                json::from_slice(&data)
                    .with_context(|| format!("invalid slot state {}", path.display()))?,
            ),
            Err(error) if error.kind() == ErrorKind::NotFound => None,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("reading slot state {}", path.display()));
            }
        };
        let slot = deadline.run("fetching slot", rpc.get_slot()).await?;
        let time = SystemTime::now().duration_since(UNIX_EPOCH)?;
        let mut sample = Sample { time, slot, strike: false };

        let Some(previous) = previous.filter(|previous| previous.time <= time) else {
            return sample.save(path);
        };
        let elapsed = time - previous.time;
        let slot_ns = slot_time.as_nanos();
        let elapsed_ns = elapsed.as_nanos();
        if elapsed_ns < 10 * slot_ns {
            return Ok(());
        }

        let advanced = i128::from(slot) - i128::from(previous.slot);
        let rate = advanced as f64 / elapsed.as_secs_f64();
        let expected_rate = 1.0 / slot_time.as_secs_f64();
        debug!(
            slot,
            advanced,
            ?elapsed,
            rate,
            expected_rate,
            "Measured slot progress"
        );

        // Compare against 90% exactly, including intervals right on the threshold.
        let minimum = (elapsed_ns * 9).div_ceil(slot_ns * 10);
        sample.strike = advanced < minimum as i128;
        sample.save(path)?;

        if sample.strike && previous.strike {
            let problem = match advanced {
                ..0 => "regressed",
                0 => "stalled",
                _ => "was slow",
            };
            bail!(
                "slot production {problem} on the second or later consecutive anomalous interval: \
             slot {} -> {slot} over {elapsed:.3?} ({rate:.2} slots/s; expected {expected_rate:.2} slots/s)",
                previous.slot,
            );
        }
        Ok(())
    }
}

impl Sample {
    fn save(&self, path: &Path) -> Result<()> {
        let directory = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let mut temp = NamedTempFile::new_in(directory)
            .with_context(|| format!("creating temporary slot state in {}", directory.display()))?;
        json::to_writer(temp.as_file_mut(), self)
            .with_context(|| format!("writing slot state {}", path.display()))?;
        temp.persist(path)
            .with_context(|| format!("replacing slot state {}", path.display()))?;
        Ok(())
    }
}
