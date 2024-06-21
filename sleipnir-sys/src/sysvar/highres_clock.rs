use serde::{Deserialize, Serialize};
use solana_sdk::{
    clock::UnixTimestamp, declare_sysvar_id, program_error::ProgramError,
    sysvar::Sysvar,
};
use solana_sdk_macro::CloneZeroed;

use crate::impl_sysvar_get;

#[repr(C)]
#[derive(
    Serialize, Deserialize, Debug, CloneZeroed, Default, PartialEq, Eq,
)]
pub struct HighresClock {
    pub validator_start: UnixTimestamp,
    pub microseconds_since_valiator_start: u64,
}

declare_sysvar_id!("SysvarHighresC1ock1111111111111111111111111", HighresClock);

impl Sysvar for HighresClock {
    impl_sysvar_get!(sol_get_highres_clock_sysvar);
}
