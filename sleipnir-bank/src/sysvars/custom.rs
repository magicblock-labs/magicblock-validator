use solana_sdk::{account::AccountSharedData, pubkey::Pubkey};

use super::highres_clock::{HighresClockWrapper, HIGHRES_CLOCK_ID};

pub struct CustomSysvars {
    highres_clock: HighresClockWrapper,
}

impl CustomSysvars {
    pub(crate) fn new(validator_identity: Pubkey) -> Self {
        let highres_clock = HighresClockWrapper::new(validator_identity);
        Self { highres_clock }
    }

    pub(crate) fn get(&self, pubkey: &Pubkey) -> Option<AccountSharedData> {
        if pubkey.eq(&HIGHRES_CLOCK_ID) {
            Some(self.highres_clock.get_account_data())
        } else {
            None
        }
    }
}
