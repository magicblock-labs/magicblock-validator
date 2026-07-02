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
/// The validator samples wall-clock time with sub-second resolution but the
/// standard `Clock` can only carry whole seconds (`unix_timestamp: i64`), so
/// that value is the high-precision timestamp rounded *down* to the second.
/// This sysvar preserves the discarded precision: `unix_timestamp` is identical
/// to `Clock::unix_timestamp` and `nanos` carries the sub-second remainder.
///
/// The full timestamp is therefore
/// `unix_timestamp as i128 * 1_000_000_000 + nanos as i128` nanoseconds.
///
/// On-chain programs can read it without passing the account, straight from the
/// sysvar cache:
///
/// ```ignore
/// use magicblock_magic_program_api::sysvar::HighPrecisionClock;
/// use solana_program::sysvar::Sysvar;
///
/// let nanos = HighPrecisionClock::get()?.nanos;
/// ```
#[derive(
    Default, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct HighPrecisionClock {
    /// Whole seconds since the Unix epoch. Identical to `Clock::unix_timestamp`.
    pub unix_timestamp: i64,
    /// Sub-second component in nanoseconds, in the range `[0, 1_000_000_000)`.
    pub nanos: u32,
}

impl HighPrecisionClock {
    /// Size in bytes of the bincode-serialized sysvar
    /// (`i64` timestamp + `u32` nanos).
    pub const SIZE: usize = 12;
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
        let unix_timestamp = i64::from_le_bytes(
            data[0..8]
                .try_into()
                .map_err(|_| ProgramError::InvalidAccountData)?,
        );
        let nanos = u32::from_le_bytes(
            data[8..12]
                .try_into()
                .map_err(|_| ProgramError::InvalidAccountData)?,
        );
        Ok(Self {
            unix_timestamp,
            nanos,
        })
    }
}

impl SysvarSerialize for HighPrecisionClock {}
