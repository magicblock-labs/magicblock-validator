#![cfg(any(test, feature = "dev-context"))]
use async_trait::async_trait;
use std::fmt;
use std::sync::Arc;

use crate::{
    accounts_bank::mock::AccountsBankStub,
    cloner::{errors::ClonerResult, Cloner},
    remote_account_provider::program_account::LoadedProgram,
};
use solana_account::AccountSharedData;
use solana_loader_v4_interface::state::LoaderV4State;
use solana_pubkey::Pubkey;
use solana_sdk::{instruction::InstructionError, signature::Signature};
use std::{collections::HashMap, sync::Mutex};

// -----------------
// Cloner
// -----------------
#[cfg(any(test, feature = "dev-context"))]
#[derive(Clone)]
pub struct ClonerStub {
    accounts_bank: Arc<AccountsBankStub>,
    cloned_programs: Arc<Mutex<HashMap<Pubkey, LoadedProgram>>>,
}

#[cfg(any(test, feature = "dev-context"))]
impl ClonerStub {
    pub fn new(accounts_bank: Arc<AccountsBankStub>) -> Self {
        Self {
            accounts_bank,
            cloned_programs:
                Arc::<Mutex<HashMap<Pubkey, LoadedProgram>>>::default(),
        }
    }

    #[allow(dead_code)]
    pub fn get_account(&self, pubkey: &Pubkey) -> Option<AccountSharedData> {
        use magicblock_core::traits::AccountsBank;

        self.accounts_bank.get_account(pubkey)
    }

    pub fn get_cloned_program(
        &self,
        program_id: &Pubkey,
    ) -> Option<LoadedProgram> {
        self.cloned_programs
            .lock()
            .unwrap()
            .get(program_id)
            .cloned()
    }

    pub fn cloned_programs_count(&self) -> usize {
        self.cloned_programs.lock().unwrap().len()
    }

    #[allow(dead_code)]
    pub fn dump_account_keys(&self, include_blacklisted: bool) -> String {
        self.accounts_bank.dump_account_keys(include_blacklisted)
    }
}

#[cfg(any(test, feature = "dev-context"))]
#[async_trait]
impl Cloner for ClonerStub {
    async fn clone_account(
        &self,
        pubkey: Pubkey,
        account: AccountSharedData,
    ) -> ClonerResult<Signature> {
        self.accounts_bank.insert(pubkey, account);
        Ok(Signature::default())
    }

    async fn clone_program(
        &self,
        program: LoadedProgram,
    ) -> ClonerResult<Signature> {
        use solana_account::WritableAccount;
        use solana_loader_v4_interface::state::LoaderV4State;
        use solana_sdk::rent::Rent;

        use crate::remote_account_provider::program_account::LOADER_V4;

        // 1. Add the program account to the bank
        {
            // Here we manually add the program account to the bank
            // In reality we will deploy the program properly with the v4 loader
            // except for v1 programs for which we will just mutate the program account

            // Serialization from:
            // https://github.com/anza-xyz/agave/blob/47c0383f2301e5a739543c1af9992ae182b7e06c/programs/loader-v4/src/lib.rs#L546
            let account_size = LoaderV4State::program_data_offset()
                .saturating_add(program.program_data.len());
            let mut program_account = AccountSharedData::new(
                Rent::default().minimum_balance(program.program_data.len()),
                account_size,
                &LOADER_V4,
            );
            let state =
                get_state_mut(program_account.data_as_mut_slice()).unwrap();
            *state = LoaderV4State {
                slot: 0,
                authority_address_or_next_version: program
                    .authority
                    .to_bytes()
                    .into(),
                status: program.loader_status,
            };
            program_account.data_as_mut_slice()
                [LoaderV4State::program_data_offset()..]
                .copy_from_slice(&program.program_data);

            program_account.set_remote_slot(program.remote_slot);
            self.accounts_bank
                .insert(program.program_id, program_account);
        }

        // 2. Also track program info for easy asserts
        {
            self.cloned_programs
                .lock()
                .unwrap()
                .insert(program.program_id, program);
        }
        Ok(Signature::default())
    }
}

fn get_state_mut(
    data: &mut [u8],
) -> Result<&mut LoaderV4State, InstructionError> {
    unsafe {
        let data = data
            .get_mut(0..LoaderV4State::program_data_offset())
            .ok_or(InstructionError::AccountDataTooSmall)?
            .try_into()
            .unwrap();
        Ok(std::mem::transmute::<
            &mut [u8; LoaderV4State::program_data_offset()],
            &mut LoaderV4State,
        >(data))
    }
}

impl fmt::Display for ClonerStub {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ClonerStub {{ \n{}", self.accounts_bank)?;
        write!(f, "\nCloned programs: [")?;
        for (k, v) in self.cloned_programs.lock().unwrap().iter() {
            write!(f, "\n  {k} => {v}")?;
        }
        write!(f, "}}")
    }
}
