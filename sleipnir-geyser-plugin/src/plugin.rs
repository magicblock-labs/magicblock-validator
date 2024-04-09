#![allow(unused)]

use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use crate::{
    config::Config,
    grpc::GrpcService,
    grpc_messages::{Message, MessageSlot},
    rpc::GeyserRpcService,
};
use log::*;
use solana_geyser_plugin_interface::geyser_plugin_interface::{
    GeyserPlugin, GeyserPluginError, ReplicaAccountInfoVersions,
    ReplicaBlockInfoVersions, ReplicaEntryInfoVersions,
    ReplicaTransactionInfoVersions, Result as PluginResult, SlotStatus,
};
use solana_sdk::{clock::Slot, pubkey::Pubkey, signature::Signature};
use stretto::Cache;
use tokio::{
    runtime::{Builder, Runtime},
    sync::{mpsc, Notify},
};

// -----------------
// PluginInner
// -----------------
#[derive(Debug)]
pub struct PluginInner {
    grpc_channel: mpsc::UnboundedSender<Message>,
    grpc_shutdown: Arc<Notify>,
    rpc_channel: mpsc::UnboundedSender<Message>,
    rpc_shutdown: Arc<Notify>,
}

impl PluginInner {
    fn send_message(&self, message: Message) {
        // TODO: If we store + send Arc<Message> we can avoid cloning here
        let _ = self.grpc_channel.send(message.clone());
        let _ = self.rpc_channel.send(message);
    }
}

// -----------------
// GrpcGeyserPlugin
// -----------------
pub struct GrpcGeyserPlugin {
    config: Config,
    inner: Option<PluginInner>,
    rpc_service: Arc<GeyserRpcService>,
    transactions_cache: Cache<Signature, Message>,
    accounts_cache: Cache<Pubkey, Message>,
}

impl std::fmt::Debug for GrpcGeyserPlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GrpcGeyserPlugin")
            .field("config", &self.config)
            .field("inner", &self.inner)
            .field("rpc_service", &self.rpc_service)
            .field("transactions_cache_size", &self.transactions_cache.len())
            .field("accounts_cache_size", &self.accounts_cache.len())
            .finish()
    }
}

impl GrpcGeyserPlugin {
    pub async fn create(config: Config) -> PluginResult<Self> {
        let (grpc_channel, grpc_shutdown) =
            GrpcService::create(config.grpc.clone(), config.block_fail_action)
                .await
                .map_err(GeyserPluginError::Custom)?;
        let transactions_cache = Cache::new(
            config.transactions_cache_num_counters,
            config.transactions_cache_max_cost,
        )
        .map_err(|err| GeyserPluginError::Custom(Box::new(err)))?;

        let accounts_cache = Cache::new(
            config.accounts_cache_num_counters,
            config.accounts_cache_max_cost,
        )
        .map_err(|err| GeyserPluginError::Custom(Box::new(err)))?;

        let (rpc_channel, rpc_shutdown, rpc_service) =
            GeyserRpcService::create(
                config.grpc.clone(),
                config.block_fail_action,
                transactions_cache.clone(),
                accounts_cache.clone(),
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
                    let message =
                        Message::Account((account, slot, is_startup).into());
                    self.accounts_cache.insert_with_ttl(
                        pubkey,
                        message.clone(),
                        1,
                        self.config.accounts_cache_ttl,
                    );
                    inner.send_message(message);
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
            let message = Message::Slot((slot, parent, status).into());
            inner.send_message(message);
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
            debug!("tx: '{}'", transaction.signature);

            let message = Message::Transaction((transaction, slot).into());
            self.transactions_cache.insert_with_ttl(
                *transaction.signature,
                // TODO: If we store + send Arc<Message> we can avoid cloning here
                message.clone(),
                1,
                self.config.transactions_cache_ttl,
            );

            // We don't call transactions_cache.wait(); here which takes about 1ms
            // to not slow down the plugin, however by the time a notification referring
            // to this transaction comes in we expect this cache update to have gone through
            inner.send_message(message);

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
        true
    }

    fn transaction_notifications_enabled(&self) -> bool {
        true
    }

    fn entry_notifications_enabled(&self) -> bool {
        false
    }
}
