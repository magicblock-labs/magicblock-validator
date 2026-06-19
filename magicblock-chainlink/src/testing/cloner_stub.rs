#![cfg(any(test, feature = "dev-context"))]
use std::{
    collections::HashMap,
    fmt,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use async_trait::async_trait;
use solana_account::AccountSharedData;
use solana_instruction::error::InstructionError;
use solana_loader_v4_interface::state::LoaderV4State;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use tokio::sync::Notify;

#[cfg(any(test, feature = "dev-context"))]
use crate::cloner::AccountCloneRequest;
use crate::{
    accounts_bank::mock::AccountsBankStub,
    cloner::{errors::ClonerResult, Cloner},
    remote_account_provider::program_account::LoadedProgram,
};

// -----------------
// Cloner
// -----------------
#[cfg(any(test, feature = "dev-context"))]
#[derive(Clone)]
pub struct ClonerStub {
    accounts_bank: Arc<AccountsBankStub>,
    cloned_programs: Arc<Mutex<HashMap<Pubkey, LoadedProgram>>>,
    clone_requests: Arc<Mutex<Vec<AccountCloneRequest>>>,
    clone_delay: Arc<Mutex<Option<Duration>>>,
    account_clone_count: Arc<AtomicU64>,
    active_account_clones: Arc<AtomicU64>,
    max_active_account_clones: Arc<AtomicU64>,
    account_clone_notify: Arc<Notify>,
    program_clone_delay: Arc<Mutex<Option<Duration>>>,
    program_clone_count: Arc<AtomicU64>,
    active_program_clones: Arc<AtomicU64>,
    max_active_program_clones: Arc<AtomicU64>,
    program_clone_notify: Arc<Notify>,
    fail_next_clone: Arc<Mutex<bool>>,
    block_clone_completion: Arc<AtomicBool>,
    clone_completion_notify: Arc<Notify>,
}

#[cfg(any(test, feature = "dev-context"))]
impl ClonerStub {
    pub fn new(accounts_bank: Arc<AccountsBankStub>) -> Self {
        Self {
            accounts_bank,
            cloned_programs:
                Arc::<Mutex<HashMap<Pubkey, LoadedProgram>>>::default(),
            clone_requests: Arc::new(Mutex::new(Vec::new())),
            clone_delay: Arc::new(Mutex::new(None)),
            account_clone_count: Arc::new(AtomicU64::new(0)),
            active_account_clones: Arc::new(AtomicU64::new(0)),
            max_active_account_clones: Arc::new(AtomicU64::new(0)),
            account_clone_notify: Arc::new(Notify::new()),
            program_clone_delay: Arc::new(Mutex::new(None)),
            program_clone_count: Arc::new(AtomicU64::new(0)),
            active_program_clones: Arc::new(AtomicU64::new(0)),
            max_active_program_clones: Arc::new(AtomicU64::new(0)),
            program_clone_notify: Arc::new(Notify::new()),
            fail_next_clone: Arc::new(Mutex::new(false)),
            block_clone_completion: Arc::new(AtomicBool::new(false)),
            clone_completion_notify: Arc::new(Notify::new()),
        }
    }

    pub fn set_fail_next_clone(&self, fail: bool) {
        *self.fail_next_clone.lock().unwrap() = fail;
    }

    pub fn set_clone_delay(&self, delay: Duration) {
        *self.clone_delay.lock().unwrap() = Some(delay);
    }

    pub fn set_program_clone_delay(&self, delay: Duration) {
        *self.program_clone_delay.lock().unwrap() = Some(delay);
    }

    pub fn block_clone_completion(&self) {
        self.block_clone_completion.store(true, Ordering::SeqCst);
    }

    pub fn allow_clone_completion(&self) {
        self.block_clone_completion.store(false, Ordering::SeqCst);
        self.clone_completion_notify.notify_waiters();
    }

    #[allow(dead_code)]
    pub fn get_account(&self, pubkey: &Pubkey) -> Option<AccountSharedData> {
        use magicblock_accounts_db::traits::AccountsBank;

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

    pub fn account_clone_count(&self) -> u64 {
        self.account_clone_count.load(Ordering::SeqCst)
    }

    pub fn max_active_account_clones(&self) -> u64 {
        self.max_active_account_clones.load(Ordering::SeqCst)
    }

    pub async fn wait_for_account_clone_count(&self, expected: u64) {
        loop {
            let notified = self.account_clone_notify.notified();
            if self.account_clone_count() >= expected {
                break;
            }
            notified.await;
        }
    }

    pub fn program_clone_count(&self) -> u64 {
        self.program_clone_count.load(Ordering::SeqCst)
    }

    pub fn max_active_program_clones(&self) -> u64 {
        self.max_active_program_clones.load(Ordering::SeqCst)
    }

    pub async fn wait_for_program_clone_count(&self, expected: u64) {
        loop {
            let notified = self.program_clone_notify.notified();
            if self.program_clone_count() >= expected {
                break;
            }
            notified.await;
        }
    }

    #[allow(dead_code)]
    pub fn dump_account_keys(&self, include_blacklisted: bool) -> String {
        self.accounts_bank.dump_account_keys(include_blacklisted)
    }

    pub fn clone_requests(&self) -> Vec<AccountCloneRequest> {
        self.clone_requests.lock().unwrap().clone()
    }

    pub fn clone_request_count(&self) -> usize {
        self.clone_requests.lock().unwrap().len()
    }

    async fn wait_for_clone_completion_allowed(&self) {
        loop {
            let notified = self.clone_completion_notify.notified();
            if !self.block_clone_completion.load(Ordering::SeqCst) {
                break;
            }
            notified.await;
        }
    }
}

#[cfg(any(test, feature = "dev-context"))]
#[async_trait]
impl Cloner for ClonerStub {
    // NOTE: commit_frequency_ms is intentionally ignored by this test stub.
    // This stub only performs cloning, not commit scheduling, so future readers/tests
    // should understand this limitation.
    async fn clone_account(
        &self,
        request: AccountCloneRequest,
    ) -> ClonerResult<Signature> {
        let active =
            self.active_account_clones.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active_account_clones
            .fetch_max(active, Ordering::SeqCst);
        self.account_clone_count.fetch_add(1, Ordering::SeqCst);
        self.account_clone_notify.notify_waiters();

        self.clone_requests.lock().unwrap().push(request.clone());
        let delay = *self.clone_delay.lock().unwrap();
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        {
            let mut should_fail = self.fail_next_clone.lock().unwrap();
            if *should_fail {
                *should_fail = false;
                self.active_account_clones.fetch_sub(1, Ordering::SeqCst);
                return Err(
                    crate::cloner::errors::ClonerError::CommittorServiceError(
                        "Injected test failure".to_string(),
                    ),
                );
            }
        }
        self.wait_for_clone_completion_allowed().await;
        self.accounts_bank.insert(request.pubkey, request.account);
        self.active_account_clones.fetch_sub(1, Ordering::SeqCst);
        Ok(Signature::default())
    }

    async fn evict_account(&self, pubkey: Pubkey) -> ClonerResult<()> {
        use magicblock_accounts_db::traits::AccountsBank;
        self.accounts_bank.remove_account(&pubkey);
        Ok(())
    }

    async fn clone_program(
        &self,
        program: LoadedProgram,
    ) -> ClonerResult<Signature> {
        use solana_account::WritableAccount;
        use solana_loader_v4_interface::state::LoaderV4State;
        use solana_program::rent::Rent;

        use crate::remote_account_provider::program_account::LOADER_V4;

        let active =
            self.active_program_clones.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active_program_clones
            .fetch_max(active, Ordering::SeqCst);
        self.program_clone_count.fetch_add(1, Ordering::SeqCst);
        self.program_clone_notify.notify_waiters();

        let delay = *self.program_clone_delay.lock().unwrap();
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        self.wait_for_clone_completion_allowed().await;

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
        self.active_program_clones.fetch_sub(1, Ordering::SeqCst);
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
