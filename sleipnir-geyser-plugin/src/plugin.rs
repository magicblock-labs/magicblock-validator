#![allow(unused)]

use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use log::*;
use solana_geyser_plugin_interface::geyser_plugin_interface::{
    GeyserPlugin, GeyserPluginError, ReplicaAccountInfoVersions,
    ReplicaBlockInfoVersions, ReplicaEntryInfoVersions,
    ReplicaTransactionInfoVersions, Result as PluginResult, SlotStatus,
};
use solana_sdk::{clock::Slot, pubkey::Pubkey};
use tokio::{
    runtime::{Builder, Runtime},
    sync::{mpsc, Notify},
};

use crate::{
    config::Config, grpc::GrpcService, grpc_messages::Message,
    rpc::GeyserRpcService,
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
        let _ = self.grpc_channel.send(message.clone());
        let _ = self.rpc_channel.send(message);
    }
}

// -----------------
// GrpcGeyserPlugin
// -----------------
#[derive(Debug)]
pub struct GrpcGeyserPlugin {
    config: Config,
    inner: Option<PluginInner>,
    rpc_service: Arc<GeyserRpcService>,
}

impl GrpcGeyserPlugin {
    pub async fn create(config: Config) -> PluginResult<Self> {
        let (grpc_channel, grpc_shutdown) =
            GrpcService::create(config.grpc.clone(), config.block_fail_action)
                .await
                .map_err(GeyserPluginError::Custom)?;
        let (rpc_channel, rpc_shutdown, rpc_service) =
            GeyserRpcService::create(
                config.grpc.clone(),
                config.block_fail_action,
            )
            .await
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
        })
    }

    pub fn rpc(&self) -> Arc<GeyserRpcService> {
        self.rpc_service.clone()
    }

    fn with_inner<F>(&self, f: F) -> PluginResult<()>
    where
        F: FnOnce(&PluginInner) -> PluginResult<()>,
    {
        if let Some(inner) = self.inner.as_ref() {
            f(inner)
        } else {
            // warn!("PluginInner is not initialized");
            Ok(())
        }
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
            let message = Message::Account((account, slot, is_startup).into());
            inner.send_message(message);

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
        Ok(())
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

            let message = Message::Transaction((transaction, slot).into());
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
