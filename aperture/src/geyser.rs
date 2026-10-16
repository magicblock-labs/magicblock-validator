use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use agave_geyser_plugin_interface::geyser_plugin_interface::{
    GeyserPlugin, GeyserPluginError, ReplicaAccountInfoV3, ReplicaAccountInfoVersions,
    ReplicaBlockInfoV4, ReplicaBlockInfoVersions, ReplicaTransactionInfoV2,
    ReplicaTransactionInfoVersions, SlotStatus,
};
use engine::Engine;
use json::{JsonValueTrait, Value};
use ledger::schema::Block;
use libloading::{Library, Symbol};
use nucleus::runtime::FullTransaction;
use solana_account::{AccountSeqLock, AccountSharedData, ReadableAccount};
use tokio::{sync::mpsc, task::JoinSet};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::{ApertureResult, transaction::processed_transaction};

const ENTRYPOINT_SYMBOL: &[u8] = b"_create_plugin";
const EVENT_QUEUE_CAPACITY: usize = 1_024;

/// Dynamic-library entrypoint; its returned plugin must be dropped before its library.
#[allow(improper_ctypes_definitions)]
type PluginCreate = unsafe extern "C" fn() -> *mut dyn GeyserPlugin;

/// Owns plugin trait objects before their libraries so plugin destructors run
/// while their defining code is still loaded.
pub(crate) struct GeyserPluginManager {
    plugins: Vec<Box<dyn GeyserPlugin>>,
    write_version: u64,
    _libs: Vec<Library>,
}

/// Feeder-order delivery across Engine's independent transaction and block streams.
enum GeyserEvent {
    Transaction(Arc<FullTransaction>),
    Block(Block),
}

impl GeyserPluginManager {
    /// Loads every valid plugin, logging individual failures so a plugin cannot
    /// prevent RPC startup or disable other plugins.
    fn load(configs: &[PathBuf]) -> Self {
        let mut manager = Self {
            plugins: Vec::with_capacity(configs.len()),
            write_version: 0,
            _libs: Vec::with_capacity(configs.len()),
        };
        for config in configs {
            // SAFETY: `load_plugin` validates the configured entrypoint and
            // keeps the returned library alive for the plugin object's lifetime.
            match unsafe { Self::load_plugin(config) } {
                Ok((plugin, library)) => {
                    manager.plugins.push(plugin);
                    manager._libs.push(library);
                }
                Err(error) => {
                    warn!(?config, ?error, "failed to load Geyser plugin")
                }
            }
        }
        manager
    }

    /// Loads one operator-configured dynamic library and adopts its plugin.
    ///
    /// # Safety
    ///
    /// The configured library must export `_create_plugin` with the exact
    /// [`PluginCreate`] ABI and return a unique heap allocation. The returned
    /// [`Library`] must outlive the plugin object.
    unsafe fn load_plugin(
        config_path: &Path,
    ) -> Result<(Box<dyn GeyserPlugin>, Library), GeyserPluginError> {
        let config = fs::read_to_string(config_path)?;
        let config: Value =
            json::from_str(&config).map_err(|error| GeyserPluginError::ConfigFileReadError {
                msg: format!("failed to parse plugin configuration: {error}"),
            })?;
        let path = config.get("libpath").and_then(JsonValueTrait::as_str).ok_or_else(|| {
            GeyserPluginError::ConfigFileReadError {
                msg: "plugin configuration must contain a string `libpath` field".into(),
            }
        })?;

        // SAFETY: loading arbitrary code is an explicit operator action through
        // `libpath`; the library handle is retained until after plugin drop.
        let library = unsafe { Library::new(path) }.map_err(|error| {
            GeyserPluginError::ConfigFileReadError {
                msg: format!("failed to load plugin library: {error}"),
            }
        })?;
        // SAFETY: the plugin ABI requires this exact symbol and function type.
        let create: Symbol<'_, PluginCreate> =
            unsafe { library.get(ENTRYPOINT_SYMBOL) }.map_err(|error| {
                GeyserPluginError::ConfigFileReadError {
                    msg: format!("failed to load plugin entrypoint: {error}"),
                }
            })?;
        // SAFETY: the ABI contract transfers ownership of one heap allocation.
        let raw = unsafe { create() };
        if raw.is_null() {
            return Err(GeyserPluginError::ConfigFileReadError {
                msg: "plugin factory returned a null pointer".into(),
            });
        }
        // SAFETY: `raw` is non-null, uniquely owned, and was allocated for the
        // trait object by the plugin factory.
        let mut plugin = unsafe { Box::from_raw(raw) };
        plugin.on_load(&config_path.to_string_lossy(), false)?;
        plugin.notify_end_of_startup()?;
        Ok((plugin, library))
    }

    fn notify_transaction(&self, transaction: &FullTransaction) {
        if !self.plugins.iter().any(|plugin| plugin.transaction_notifications_enabled()) {
            return;
        }
        let slot = transaction.execution.slot;
        let Ok((sanitized, meta)) = processed_transaction(transaction)
            .inspect_err(|error| warn!(?error, "failed to convert engine transaction for Geyser"))
        else {
            return;
        };
        let signature = &sanitized.signatures()[0];
        let info = ReplicaTransactionInfoV2 {
            signature,
            is_vote: sanitized.is_simple_vote_transaction(),
            transaction: &sanitized,
            transaction_status_meta: &meta,
            // The engine does not currently retain an intra-block index.
            index: 0,
        };
        for plugin in &self.plugins {
            if plugin.transaction_notifications_enabled()
                && let Err(error) =
                    plugin.notify_transaction(ReplicaTransactionInfoVersions::V0_0_2(&info), slot)
            {
                warn!(
                    plugin = plugin.name(),
                    ?error,
                    "Geyser transaction notification failed"
                );
            }
        }
    }

    fn notify_accounts(&mut self, transaction: &FullTransaction) {
        if !self.plugins.iter().any(|plugin| plugin.account_data_notifications_enabled()) {
            return;
        }
        let slot = transaction.execution.slot;
        let Ok(execution) = &transaction.execution.result else {
            return;
        };
        // Failed executions do not publish committed account changes.
        if !execution.was_successful() {
            return;
        }
        for (pubkey, account) in &execution.loaded_transaction.accounts {
            // Snapshot dirty images inside the scoped seqlock read.
            let Some(account) = AccountSeqLock::new(account.clone())
                .read(|account| account.dirty().then(|| AccountSharedData::from(account.owned())))
            else {
                continue;
            };
            let write_version = self.write_version;
            self.write_version += 1;
            let info = ReplicaAccountInfoV3 {
                pubkey: pubkey.as_array(),
                lamports: account.lamports(),
                owner: account.owner().as_array(),
                executable: account.executable(),
                rent_epoch: account.rent_epoch(),
                data: account.data(),
                write_version,
                // Engine account entries do not retain transaction context.
                txn: None,
            };
            for plugin in &self.plugins {
                if plugin.account_data_notifications_enabled()
                    && let Err(error) = plugin.update_account(
                        ReplicaAccountInfoVersions::V0_0_3(&info),
                        slot,
                        false,
                    )
                {
                    warn!(
                        plugin = plugin.name(),
                        ?error,
                        "Geyser account notification failed"
                    );
                }
            }
        }
    }

    fn notify_block(&self, block: Block) {
        let parent = block.slot.checked_sub(1);
        for plugin in &self.plugins {
            if let Err(error) = plugin.update_slot_status(block.slot, parent, &SlotStatus::Rooted) {
                warn!(
                    plugin = plugin.name(),
                    ?error,
                    "Geyser slot notification failed"
                );
            }
        }

        let blockhash = block.hash.to_string();
        let parent_blockhash = block.parent.to_string();
        let rewards = solana_transaction_status::RewardsAndNumPartitions {
            rewards: Vec::new(),
            num_partitions: None,
        };
        let info = ReplicaBlockInfoV4 {
            slot: block.slot,
            parent_slot: block.slot.saturating_sub(1),
            blockhash: &blockhash,
            block_height: Some(block.slot),
            rewards: &rewards,
            block_time: Some(block.time),
            parent_blockhash: &parent_blockhash,
            // Engine does not yet retain transaction/entry counts.
            executed_transaction_count: 0,
            entry_count: 0,
        };
        for plugin in &self.plugins {
            if let Err(error) =
                plugin.notify_block_metadata(ReplicaBlockInfoVersions::V0_0_4(&info))
            {
                warn!(
                    plugin = plugin.name(),
                    ?error,
                    "Geyser block notification failed"
                );
            }
        }
    }
}

impl Drop for GeyserPluginManager {
    fn drop(&mut self) {
        for plugin in &mut self.plugins {
            plugin.on_unload();
        }
    }
}

/// Prepared Engine subscriptions; no delivery runs until Aperture::run.
pub(crate) struct Delivery {
    manager: GeyserPluginManager,
    transactions: mpsc::UnboundedReceiver<Arc<FullTransaction>>,
    blocks: mpsc::Receiver<Block>,
}

/// Loads plugins and opens subscriptions without spawning tasks; empty managers do no work.
pub(crate) fn prepare(configs: &[PathBuf], engine: Engine) -> ApertureResult<Option<Delivery>> {
    Ok(prepare_manager(GeyserPluginManager::load(configs), engine)?)
}

/// Acquires Engine streams only when there are loaded plugins to receive them.
fn prepare_manager(
    manager: GeyserPluginManager,
    engine: Engine,
) -> Result<Option<Delivery>, keeper::error::KeeperError> {
    if manager.plugins.is_empty() {
        return Ok(None);
    }
    Ok(Some(Delivery {
        manager,
        transactions: engine.transactions().subscribe_processed()?,
        blocks: engine.blocks().subscribe(),
    }))
}

impl Delivery {
    /// Serial callbacks preserve feeder order, not cross-stream Engine ordering.
    pub(crate) fn spawn(self, tasks: &mut JoinSet<()>, cancel: CancellationToken) {
        let Self {
            mut manager,
            mut transactions,
            mut blocks,
        } = self;
        let (tx, mut rx) = mpsc::channel(EVENT_QUEUE_CAPACITY);
        let feeder_cancel = cancel.clone();
        tasks.spawn(async move {
            loop {
                let event = tokio::select! {
                    _ = feeder_cancel.cancelled() => break,
                    result = transactions.recv() => match result { Some(tx) => GeyserEvent::Transaction(tx), None => break },
                    result = blocks.recv() => match result { Some(block) => GeyserEvent::Block(block), None => break },
                };
                tokio::select! {
                    _ = feeder_cancel.cancelled() => break,
                    result = tx.send(event) => if result.is_err() { break; },
                }
            }
        });
        tasks.spawn(async move {
            loop {
                let event = tokio::select! {
                    _ = cancel.cancelled() => break,
                    event = rx.recv() => event,
                };
                match event {
                    Some(GeyserEvent::Transaction(transaction)) => {
                        manager.notify_transaction(&transaction);
                        manager.notify_accounts(&transaction);
                    }
                    Some(GeyserEvent::Block(block)) => manager.notify_block(block),
                    None => break,
                }
            }
        });
    }
}
