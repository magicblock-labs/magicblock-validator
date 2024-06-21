use lazy_static::lazy_static;
use serde::{Deserialize, Serialize};
use solana_sdk::{
    account::{Account, AccountSharedData},
    clock::Epoch,
    pubkey::Pubkey,
};
use std::{str::FromStr, sync::RwLock, time::SystemTime};

pub const HIGHRES_CLOCK_ADDR: &str =
    "SysvarHighresC1ock1111111111111111111111111";

lazy_static! {
    pub static ref HIGHRES_CLOCK_ID: Pubkey =
        Pubkey::from_str(HIGHRES_CLOCK_ADDR).unwrap();
    pub static ref HIGHRES_CLOCK_SIZE: usize =
        bincode::serialized_size(&0_u128).unwrap() as usize;
}

pub struct HighresClockWrapper {
    /// Epoch time when validator started
    validator_start: SystemTime,
    highres_clock_account: RwLock<AccountSharedData>,
    validator_identity: Pubkey,
}

impl std::fmt::Debug for HighresClockWrapper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HighresClockWrapper")
            .field("validator_start", &self.validator_start)
            .field("validator_identity", &self.validator_identity)
            .finish()
    }
}

impl HighresClockWrapper {
    pub(super) fn new(validator_identity: Pubkey) -> Self {
        let highres_clock = HighresClock::init();
        let account_data = highres_clock.get_account_data(&validator_identity);
        Self {
            validator_start: SystemTime::now(),
            highres_clock_account: RwLock::new(account_data),
            validator_identity,
        }
    }

    fn micros_since_validator_start(&self) -> u128 {
        SystemTime::now()
            .duration_since(self.validator_start)
            .unwrap()
            .as_micros()
    }

    pub(crate) fn update(&self) {
        let highres_clock =
            HighresClock::new(self.micros_since_validator_start());
        let account_data =
            highres_clock.get_account_data(&self.validator_identity);
        *self.highres_clock_account.write().unwrap() = account_data;
    }

    pub(super) fn get_account_data(&self) -> AccountSharedData {
        self.highres_clock_account.read().unwrap().clone()
    }

    #[cfg(test)]
    fn get_deserialized_account_data(&self) -> HighresClock {
        use solana_sdk::account::ReadableAccount;
        HighresClock::deserialize(self.get_account_data().data())
    }
}

#[derive(Default, Serialize, Deserialize, Debug, PartialEq, Eq)]
struct HighresClock {
    /// Microseconds that passed since the validator started
    pub micros_since_valiator_start: Option<u128>,
}

impl HighresClock {
    pub fn init() -> Self {
        // Set it to non-zero to indicate that the validator supports a highres clock
        Self::new(1)
    }

    fn new(micros_since_valiator_start: u128) -> Self {
        Self {
            micros_since_valiator_start: Some(micros_since_valiator_start),
        }
    }

    fn get_account_data(&self, identity_pubkey: &Pubkey) -> AccountSharedData {
        let data = self.serialize();
        let account = Account {
            lamports: 1,
            data,
            owner: *identity_pubkey,
            executable: false,
            rent_epoch: Epoch::default(),
        };
        AccountSharedData::from(account)
    }

    fn serialize(&self) -> Vec<u8> {
        let mut data = Vec::with_capacity(*HIGHRES_CLOCK_SIZE);
        match self.micros_since_valiator_start {
            Some(micros_since_valiator_start) => {
                bincode::serialize_into(
                    &mut data,
                    &micros_since_valiator_start,
                )
                .unwrap();
            }
            None => {
                bincode::serialize_into(&mut data, &0_u128).unwrap();
            }
        }
        data
    }

    #[cfg(test)]
    fn deserialize(data: &[u8]) -> Self {
        // NOTE: we take the presence 0 micros as an indicator that the validator
        // does not support a highres clock
        match bincode::deserialize(data).unwrap() {
            0 => Self::default(),
            micros_since_valiator_start => Self {
                micros_since_valiator_start: Some(micros_since_valiator_start),
            },
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_highres_clock() {
        let identity = Pubkey::new_unique();
        let highres_clock_wrapper = HighresClockWrapper::new(identity);
        let highres_clock =
            highres_clock_wrapper.get_deserialized_account_data();
        assert_eq!(highres_clock.micros_since_valiator_start, Some(1));

        highres_clock_wrapper.update();
        let highres_clock =
            highres_clock_wrapper.get_deserialized_account_data();
        assert!(highres_clock.micros_since_valiator_start.unwrap() > 1);
    }
}
