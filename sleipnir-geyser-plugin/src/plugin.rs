#![allow(unused)]

use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use circular_hashmap::CircularHashMap as Cache;
use log::*;
use solana_geyser_plugin_interface::geyser_plugin_interface::{
    GeyserPlugin, GeyserPluginError, ReplicaAccountInfoVersions,
    ReplicaBlockInfoVersions, ReplicaEntryInfoVersions,
    ReplicaTransactionInfoVersions, Result as PluginResult, SlotStatus,
};
use solana_sdk::{clock::Slot, pubkey::Pubkey, signature::Signature};
use tokio::{
    runtime::{Builder, Runtime},
    sync::{mpsc, Notify},
};

use crate::{
    config::Config,
    grpc::GrpcService,
    grpc_messages::{Message, MessageSlot},
    rpc::GeyserRpcService,
    types::GeyserMessage,
    utils::CacheState,
};

// -----------------
// PluginInner
// -----------------
#[derive(Debug)]
pub struct PluginInner {
    grpc_channel: mpsc::UnboundedSender<GeyserMessage>,
    grpc_shutdown: Arc<Notify>,
    rpc_channel: mpsc::UnboundedSender<GeyserMessage>,
    rpc_shutdown: Arc<Notify>,
}

impl PluginInner {
    fn send_message(&self, message: &GeyserMessage) {
        let _ = self.grpc_channel.send(message.clone());
        let _ = self.rpc_channel.send(message.clone());
    }
}

// -----------------
// GrpcGeyserPlugin
// -----------------
pub struct GrpcGeyserPlugin {
    config: Config,
    inner: Option<PluginInner>,
    rpc_service: Arc<GeyserRpcService>,
    transactions_cache: Option<Cache<Signature, GeyserMessage>>,
    accounts_cache: Option<Cache<Pubkey, GeyserMessage>>,
}

impl std::fmt::Debug for GrpcGeyserPlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let tx_cache = CacheState::from(self.transactions_cache.as_ref());
        let acc_cache = CacheState::from(self.accounts_cache.as_ref());
        f.debug_struct("GrpcGeyserPlugin")
            .field("config", &self.config)
            .field("inner", &self.inner)
            .field("rpc_service", &self.rpc_service)
            .field("transactions_cache", &tx_cache)
            .field("accounts_cache", &acc_cache)
            .finish()
    }
}

impl GrpcGeyserPlugin {
    pub async fn create(config: Config) -> PluginResult<Self> {
        let (grpc_channel, grpc_shutdown) =
            GrpcService::create(config.grpc.clone(), config.block_fail_action)
                .await
                .map_err(GeyserPluginError::Custom)?;

        let transactions_cache = if config.cache_transactions {
            Some(Cache::new(config.transactions_cache_max_cached_items))
        } else {
            None
        };

        let accounts_cache = if config.cache_accounts {
            Some(Cache::new(config.accounts_cache_max_cached_items))
        } else {
            None
        };

        let (rpc_channel, rpc_shutdown, rpc_service) =
            GeyserRpcService::create(
                config.grpc.clone(),
                config.block_fail_action,
                transactions_cache.as_ref().map(|x| x.shared_map()),
                accounts_cache.as_ref().map(|x| x.shared_map()),
            )
            .map_err(GeyserPluginError::Custom)?;
        let rpc_service = Arc::new(rpc_service);
        let inner = Some(PluginInner {
            grpc_channel,
            grpc_shutdown,
            rpc_channel,
            rpc_shutdown,
        });

        Ok(Self {
            config,
            inner,
            rpc_service,
            transactions_cache,
            accounts_cache,
        })
    }

    pub fn rpc(&self) -> Arc<GeyserRpcService> {
        self.rpc_service.clone()
    }

    fn with_inner<F>(&self, f: F) -> PluginResult<()>
    where
        F: FnOnce(&PluginInner) -> PluginResult<()>,
    {
        let inner =
            self.inner.as_ref().expect("PluginInner is not initialized");
        f(inner)
    }
}

impl GeyserPlugin for GrpcGeyserPlugin {
    fn name(&self) -> &'static str {
        concat!(env!("CARGO_PKG_NAME"), "-", env!("CARGO_PKG_VERSION"))
    }

    fn on_load(
        &mut self,
        _config_file: &str,
        _is_reload: bool,
    ) -> PluginResult<()> {
        info!("Loaded plugin: {}", self.name());
        Ok(())
    }

    fn on_unload(&mut self) {
        if let Some(inner) = self.inner.take() {
            inner.grpc_shutdown.notify_one();
            inner.rpc_shutdown.notify_one();
            drop(inner.grpc_channel);
            drop(inner.rpc_channel);
        }
        info!("Unoaded plugin: {}", self.name());
    }

    fn update_account(
        &self,
        account: ReplicaAccountInfoVersions,
        slot: Slot,
        is_startup: bool,
    ) -> PluginResult<()> {
        if is_startup {
            return Ok(());
        }
        self.with_inner(|inner| {
            let account = match account {
                ReplicaAccountInfoVersions::V0_0_1(_info) => {
                    unreachable!(
                        "ReplicaAccountInfoVersions::V0_0_1 is not supported"
                    )
                }
                ReplicaAccountInfoVersions::V0_0_2(_info) => {
                    unreachable!(
                        "ReplicaAccountInfoVersions::V0_0_2 is not supported"
                    )
                }
                ReplicaAccountInfoVersions::V0_0_3(info) => info,
            };

            match Pubkey::try_from(account.pubkey) {
                Ok(pubkey) => {
                    let message = Arc::new(Message::Account(
                        (account, slot, is_startup).into(),
                    ));
                    if let Some(accounts_cache) = self.accounts_cache.as_ref() {
                        accounts_cache.insert(pubkey, message.clone());

                        if let Some(interval) =
                            std::option_env!("DIAG_GEYSER_ACC_CACHE_INTERVAL")
                        {
                            let interval = interval.parse::<usize>().unwrap();
                            if !accounts_cache.contains_key(&pubkey) {
                                error!(
                                    "Account not cached '{}', cache size {}",
                                    pubkey,
                                    accounts_cache.len()
                                );
                            }

                            static COUNTER: AtomicUsize = AtomicUsize::new(0);
                            let count = COUNTER.fetch_add(1, Ordering::SeqCst);
                            if count % interval == 0 {
                                info!(
                                    "AccountsCache size: {}, accounts: {}",
                                    accounts_cache.len(),
                                    count,
                                );
                            }
                        }
                    }
                    inner.send_message(&message);
                }
                Err(err) => error!(
                    "Encountered invalid pubkey for account update: {}",
                    err
                ),
            };

            Ok(())
        })
    }

    fn notify_end_of_startup(&self) -> PluginResult<()> {
        debug!("End of startup");
        Ok(())
    }

    fn update_slot_status(
        &self,
        slot: Slot,
        parent: Option<u64>,
        status: SlotStatus,
    ) -> PluginResult<()> {
        self.with_inner(|inner| {
            let message =
                Arc::new(Message::Slot((slot, parent, status).into()));
            inner.send_message(&message);
            Ok(())
        })
    }

    fn notify_transaction(
        &self,
        transaction: ReplicaTransactionInfoVersions,
        slot: Slot,
    ) -> PluginResult<()> {
        self.with_inner(|inner| {
            let transaction = match transaction {
                ReplicaTransactionInfoVersions::V0_0_1(_info) => {
                    unreachable!(
                        "ReplicaAccountInfoVersions::V0_0_1 is not supported"
                    )
                }
                ReplicaTransactionInfoVersions::V0_0_2(info) => info,
            };
            trace!("tx: '{}'", transaction.signature);

            let message =
                Arc::new(Message::Transaction((transaction, slot).into()));
            if let Some(transactions_cache) = self.transactions_cache.as_ref() {
                transactions_cache
                    .insert(*transaction.signature, message.clone());

                if let Some(interval) =
                    std::option_env!("DIAG_GEYSER_TX_CACHE_INTERVAL")
                {
                    let interval = interval.parse::<usize>().unwrap();
                    if !transactions_cache.contains_key(transaction.signature) {
                        let sig = crate::utils::short_signature(
                            transaction.signature,
                        );
                        error!(
                            "Item not cached '{}', cache size {}",
                            sig,
                            transactions_cache.len()
                        );
                    }

                    static COUNTER: AtomicUsize = AtomicUsize::new(0);
                    let count = COUNTER.fetch_add(1, Ordering::SeqCst);
                    if count % interval == 0 {
                        info!(
                            "TransactionCache size: {}, transactions: {}",
                            transactions_cache.len(),
                            count
                        );
                    }
                }
            }

            inner.send_message(&message);

            Ok(())
        })
    }

    fn notify_entry(
        &self,
        entry: ReplicaEntryInfoVersions,
    ) -> PluginResult<()> {
        Ok(())
    }

    fn notify_block_metadata(
        &self,
        blockinfo: ReplicaBlockInfoVersions,
    ) -> PluginResult<()> {
        Ok(())
    }

    fn account_data_notifications_enabled(&self) -> bool {
        self.config.enable_account_notifications
    }

    fn transaction_notifications_enabled(&self) -> bool {
        self.config.enable_transaction_notifications
    }

    fn entry_notifications_enabled(&self) -> bool {
        false
    }
}
