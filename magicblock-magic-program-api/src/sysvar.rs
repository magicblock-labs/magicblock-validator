//! MagicBlock-specific sysvars.

use serde::{Deserialize, Serialize};
use solana_program::program_error::ProgramError;
use solana_sysvar::{get_sysvar, Sysvar, SysvarSerialize};
use solana_sysvar_id::SysvarId;

use crate::{pubkey, Pubkey};

/// Address of the [`HighPrecisionClock`] sysvar.
///
/// This is a MagicBlock-specific sysvar and has no counterpart on Solana
/// mainnet. It is owned by the sysvar program like every other sysvar.
pub const HIGH_PRECISION_CLOCK_ID: Pubkey =
    pubkey!("SysvarHighPrecisionC1ock1111111111111111111");

/// A high-precision companion to the [`Clock`](solana_program::clock::Clock)
/// sysvar.
///
/// The standard `Clock` can only carry whole seconds (`unix_timestamp: i64`),
/// discarding the sub-second precision the validator samples from wall-clock
/// time. This sysvar exposes the same timestamp with **millisecond** accuracy:
/// `unix_timestamp_millis` is the Unix time in milliseconds, and
/// `unix_timestamp_millis / 1000` equals `Clock::unix_timestamp`.
///
/// On-chain programs can read it without passing the account, straight from the
/// sysvar cache:
///
/// ```ignore
/// use magicblock_magic_program_api::sysvar::HighPrecisionClock;
/// use solana_program::sysvar::Sysvar;
///
/// let millis = HighPrecisionClock::get()?.unix_timestamp_millis;
/// ```
#[derive(
    Default, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct HighPrecisionClock {
    /// Milliseconds since the Unix epoch.
    pub unix_timestamp_millis: i64,
}

impl HighPrecisionClock {
    /// Size in bytes of the bincode-serialized sysvar (a single `i64`).
    pub const SIZE: usize = 8;
}

impl SysvarId for HighPrecisionClock {
    fn id() -> Pubkey {
        HIGH_PRECISION_CLOCK_ID
    }

    fn check_id(id: &Pubkey) -> bool {
        id == &HIGH_PRECISION_CLOCK_ID
    }
}

impl Sysvar for HighPrecisionClock {
    /// Reads the sysvar directly from the runtime cache via the `sol_get_sysvar`
    /// syscall, without requiring the account to be passed to the program.
    fn get() -> Result<Self, ProgramError> {
        let mut data = [0u8; Self::SIZE];
        get_sysvar(&mut data, &HIGH_PRECISION_CLOCK_ID, 0, Self::SIZE as u64)?;
        let unix_timestamp_millis = i64::from_le_bytes(
            data.try_into()
                .map_err(|_| ProgramError::InvalidAccountData)?,
        );
        Ok(Self {
            unix_timestamp_millis,
        })
    }
}

impl SysvarSerialize for HighPrecisionClock {}
