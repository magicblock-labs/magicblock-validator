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
    config::Config,
    grpc::{GrpcService, Message},
};

// -----------------
// PluginInner
// -----------------
#[derive(Debug)]
pub struct PluginInner {
    runtime: Runtime,
    grpc_channel: mpsc::UnboundedSender<Message>,
    grpc_shutdown: Arc<Notify>,
}

impl PluginInner {
    fn send_message(&self, message: Message) {
        let _ = self.grpc_channel.send(message);
    }
}

// -----------------
// GrpcGeyserPlugin
// -----------------
#[derive(Debug, Default)]
pub struct GrpcGeyserPlugin {
    config: Config,
    inner: Option<PluginInner>,
}

impl GrpcGeyserPlugin {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            inner: None,
        }
    }

    fn with_inner<F>(&self, f: F) -> PluginResult<()>
    where
        F: FnOnce(&PluginInner) -> PluginResult<()>,
    {
        let inner = self.inner.as_ref().expect("initialized");
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
        let config = &self.config;

        // Create inner
        let runtime = Builder::new_multi_thread()
            .thread_name_fn(|| {
                static ATOMIC_ID: AtomicUsize = AtomicUsize::new(0);
                let id = ATOMIC_ID.fetch_add(1, Ordering::Relaxed);
                format!("solGeyserGrpc{id:02}")
            })
            .enable_all()
            .build()
            .map_err(|error| GeyserPluginError::Custom(Box::new(error)))?;

        let (grpc_channel, grpc_shutdown) = runtime.block_on(async move {
            let (grpc_channel, grpc_shutdown) = GrpcService::create(
                config.grpc.clone(),
                config.block_fail_action,
            )
            .await
            .map_err(GeyserPluginError::Custom)?;
            Ok::<_, GeyserPluginError>((grpc_channel, grpc_shutdown))
        })?;

        self.inner = Some(PluginInner {
            runtime,
            grpc_channel,
            grpc_shutdown,
        });

        Ok(())
    }

    fn on_unload(&mut self) {
        if let Some(inner) = self.inner.take() {
            inner.grpc_shutdown.notify_one();
            drop(inner.grpc_channel);
            inner.runtime.shutdown_timeout(Duration::from_secs(30));
        }
    }

    fn update_account(
        &self,
        account: ReplicaAccountInfoVersions,
        slot: Slot,
        is_startup: bool,
    ) -> PluginResult<()> {
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

        if account.txn.is_some() {
            debug!(
                "update_account '{}': {:?}",
                Pubkey::try_from(account.pubkey).unwrap(),
                account
            );
        }
        Ok(())
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
        let transaction = match transaction {
            ReplicaTransactionInfoVersions::V0_0_1(_info) => {
                unreachable!(
                    "ReplicaAccountInfoVersions::V0_0_1 is not supported"
                )
            }
            ReplicaTransactionInfoVersions::V0_0_2(info) => info,
        };

        debug!("notify_transaction: {:?}", transaction);
        Ok(())
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
