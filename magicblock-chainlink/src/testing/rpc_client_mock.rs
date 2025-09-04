#[cfg(any(test, feature = "dev-context"))]
use async_trait::async_trait;
#[cfg(any(test, feature = "dev-context"))]
use log::*;
#[cfg(any(test, feature = "dev-context"))]
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};

#[cfg(any(test, feature = "dev-context"))]
use solana_account::Account;
#[cfg(any(test, feature = "dev-context"))]
use solana_pubkey::Pubkey;
#[cfg(any(test, feature = "dev-context"))]
use solana_rpc_client_api::{
    client_error::Result as ClientResult,
    config::RpcAccountInfoConfig,
    response::{Response, RpcResponseContext, RpcResult},
};
#[cfg(any(test, feature = "dev-context"))]
use solana_sdk::{commitment_config::CommitmentConfig, sysvar::clock};

#[cfg(any(test, feature = "dev-context"))]
use crate::remote_account_provider::chain_rpc_client::ChainRpcClient;

#[cfg(any(test, feature = "dev-context"))]
pub struct ChainRpcClientMockBuilder {
    commitment: CommitmentConfig,
    accounts: HashMap<Pubkey, AccountAtSlot>,
    current_slot: u64,
    clock_sysvar: Option<clock::Clock>,
}

#[cfg(any(test, feature = "dev-context"))]
impl Default for ChainRpcClientMockBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(any(test, feature = "dev-context"))]
impl ChainRpcClientMockBuilder {
    pub fn new() -> Self {
        Self {
            commitment: CommitmentConfig::confirmed(),
            accounts: HashMap::new(),
            current_slot: 0,
            clock_sysvar: None,
        }
    }

    pub fn commitment(mut self, commitment: CommitmentConfig) -> Self {
        self.commitment = commitment;
        self
    }

    /// Sets the slot of the remote validator.
    /// It also updates the clock sysvar to match the slot as well as makes
    /// all stored accounts available at this slot.
    /// Use [Self::clock_sysvar_for_slot] and [Self::account_override_slot] respectively
    /// to fine tune this in order to simulate RPC staleness scenarios.
    pub fn slot(mut self, slot: u64) -> Self {
        self.current_slot = slot;
        for account in self.accounts.values_mut() {
            account.slot = slot;
        }
        self.clock_sysvar_for_slot(slot)
    }

    pub fn clock_sysvar_for_slot(mut self, slot: u64) -> Self {
        self.clock_sysvar.replace(clock::Clock {
            slot,
            ..Default::default()
        });
        self
    }

    /// Overrides the slot for which an account is available which allows simulating RPC account
    /// staleness issues.
    /// Make sure to call this last since methods like [Self::slot] will override the slot of all
    /// accounts.
    pub fn account_override_slot(mut self, pubkey: &Pubkey, slot: u64) -> Self {
        if let Some(account) = self.accounts.get_mut(pubkey) {
            account.slot = slot;
        } else {
            warn!("Account {pubkey} not found in mock accounts");
        }
        self
    }

    pub fn accounts(self, accounts: HashMap<Pubkey, Account>) -> Self {
        let mut me = self;
        for (pubkey, account) in accounts {
            me = me.account(pubkey, account);
        }
        me
    }

    pub fn account(mut self, pubkey: Pubkey, account: Account) -> Self {
        let slot = self.current_slot;
        self.accounts
            .insert(pubkey, AccountAtSlot { account, slot });
        self
    }

    pub fn build(self) -> ChainRpcClientMock {
        let mock = ChainRpcClientMock {
            commitment: self.commitment,
            accounts: Arc::new(Mutex::new(self.accounts)),
            current_slot: Arc::new(AtomicU64::new(self.current_slot)),
        };
        if let Some(clock_sysvar) = self.clock_sysvar {
            mock.set_clock_sysvar(clock_sysvar);
        }
        mock
    }
}

#[cfg(any(test, feature = "dev-context"))]
#[derive(Clone)]
pub struct AccountAtSlot {
    pub account: Account,
    pub slot: u64,
}

#[cfg(any(test, feature = "dev-context"))]
#[derive(Clone)]
pub struct ChainRpcClientMock {
    commitment: CommitmentConfig,
    accounts: Arc<Mutex<HashMap<Pubkey, AccountAtSlot>>>,
    current_slot: Arc<AtomicU64>,
}

#[cfg(any(test, feature = "dev-context"))]
impl ChainRpcClientMock {
    pub fn new(commitment: CommitmentConfig) -> Self {
        Self {
            commitment,
            accounts: Arc::new(Mutex::new(HashMap::new())),
            current_slot: Arc::<AtomicU64>::default(),
        }
    }

    pub fn get_slot(&self) -> u64 {
        self.current_slot.load(Ordering::Relaxed)
    }

    /// Sets current slot and updates the clock sysvar to match it.
    /// It also updates all accounts to be available at that slot.
    /// In order to simulate RPC staleness issues, use [Self::account_override_slot] as well as
    /// [Self::set_clock_sysvar_for_slot].
    pub fn set_slot(&self, slot: u64) -> u64 {
        trace!("Setting slot to {slot}");
        self.current_slot.store(slot, Ordering::Relaxed);
        for account in self.accounts.lock().unwrap().values_mut() {
            account.slot = slot;
        }
        slot
    }

    pub fn set_clock_sysvar_for_slot(&self, slot: u64) {
        self.set_clock_sysvar_with(slot, 0, 0);
    }

    pub fn set_clock_sysvar(&self, clock: clock::Clock) {
        trace!("Setting clock sysvar: {clock:?}");
        let clock_data = bincode::serialize(&clock).unwrap();
        let account = Account {
            lamports: 1_000_000_000,
            data: clock_data,
            owner: clock::id(),
            ..Default::default()
        };
        self.add_account(clock::id(), account);
        self.account_override_slot(&clock::id(), clock.slot);
    }

    pub fn set_clock_sysvar_with(
        &self,
        slot: u64,
        epoch: u64,
        leader_schedule_epoch: u64,
    ) {
        trace!(
            "Adding clock sysvar with slot {slot}, epoch {epoch}, leader_schedule_epoch {leader_schedule_epoch}"
        );
        let clock = clock::Clock {
            slot,
            epoch,
            leader_schedule_epoch,
            ..Default::default()
        };
        self.set_clock_sysvar(clock);
    }

    pub fn account_override_slot(&self, pubkey: &Pubkey, slot: u64) {
        trace!("Overriding slot for account {pubkey} to {slot}");
        let mut lock = self.accounts.lock().unwrap();
        if let Some(account) = lock.get_mut(pubkey) {
            account.slot = slot;
        } else {
            warn!("Account {pubkey} not found in mock accounts");
        }
    }

    pub fn add_account(&self, pubkey: Pubkey, account: Account) {
        let slot = self.current_slot.load(Ordering::Relaxed);
        trace!("Adding account {pubkey} at slot {slot}");
        self.accounts
            .lock()
            .unwrap()
            .insert(pubkey, AccountAtSlot { account, slot });
    }

    pub fn remove_account(&self, pubkey: &Pubkey) {
        trace!("Removing account {pubkey}");
        self.accounts.lock().unwrap().remove(pubkey);
    }

    pub fn get_account_at_slot(
        &self,
        pubkey: &Pubkey,
    ) -> Option<AccountAtSlot> {
        trace!("Getting account for pubkey {pubkey}");
        let lock = self.accounts.lock().unwrap();
        let acc = lock.get(pubkey)?;
        if acc.slot >= self.current_slot.load(Ordering::Relaxed) {
            Some(acc.clone())
        } else {
            None
        }
    }

    pub fn set_current_slot(&self, slot: u64) {
        trace!("Setting current slot to {slot}");
        self.current_slot.store(slot, Ordering::Relaxed);
    }
}

#[cfg(any(test, feature = "dev-context"))]
impl Default for ChainRpcClientMock {
    fn default() -> Self {
        Self::new(CommitmentConfig::confirmed())
    }
}

#[cfg(any(test, feature = "dev-context"))]
#[async_trait]
impl ChainRpcClient for ChainRpcClientMock {
    fn commitment(&self) -> CommitmentConfig {
        self.commitment
    }

    async fn get_account_with_config(
        &self,
        pubkey: &Pubkey,
        _config: RpcAccountInfoConfig,
    ) -> RpcResult<Option<Account>> {
        let res = if let Some(AccountAtSlot { account, slot }) =
            self.get_account_at_slot(pubkey)
        {
            Response {
                context: RpcResponseContext {
                    slot,
                    api_version: None,
                },
                value: Some(account),
            }
        } else {
            Response {
                context: RpcResponseContext {
                    slot: self.current_slot.load(Ordering::Relaxed),
                    api_version: None,
                },
                value: None,
            }
        };

        Ok(res)
    }

    async fn get_multiple_accounts_with_config(
        &self,
        pubkeys: &[Pubkey],
        config: RpcAccountInfoConfig,
    ) -> RpcResult<Vec<Option<Account>>> {
        if log::log_enabled!(log::Level::Trace) {
            let pubkeys = pubkeys
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            trace!("get_multiple_accounts_with_config({pubkeys})");
        }
        let mut accounts = vec![];
        for pubkey in pubkeys {
            let val = self
                .get_account_with_config(pubkey, config.clone())
                .await
                .unwrap()
                .value;
            accounts.push(val);
        }

        let res = Response {
            context: RpcResponseContext {
                slot: self.current_slot.load(Ordering::Relaxed),
                api_version: None,
            },
            value: accounts,
        };
        Ok(res)
    }

    async fn get_slot_with_commitment(
        &self,
        _commitment: CommitmentConfig,
    ) -> ClientResult<u64> {
        todo!("Implement get_slot_with_commitment for ChainRpcClientMock");
    }
}
