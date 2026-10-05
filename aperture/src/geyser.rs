use std::{
    fs,
    path::{Path, PathBuf},
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
    Transaction(FullTransaction),
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

    fn notify_transaction(&self, transaction: &mut FullTransaction) {
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

    fn notify_accounts(&mut self, transaction: FullTransaction) {
        if !self.plugins.iter().any(|plugin| plugin.account_data_notifications_enabled()) {
            return;
        }
        let slot = transaction.execution.slot;
        let Ok(execution) = transaction.execution.result else {
            return;
        };
        // Failed executions do not publish committed account changes.
        if !execution.was_successful() {
            return;
        }
        for (pubkey, account) in execution.loaded_transaction.accounts {
            // Snapshot dirty images inside the scoped seqlock read.
            let Some(account) = AccountSeqLock::new(account)
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

    #[cfg(test)]
    fn from_plugins(plugins: Vec<Box<dyn GeyserPlugin>>) -> Self {
        Self {
            plugins,
            write_version: 0,
            _libs: Vec::new(),
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
    transactions: mpsc::UnboundedReceiver<FullTransaction>,
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
                    Some(GeyserEvent::Transaction(mut transaction)) => {
                        manager.notify_transaction(&mut transaction);
                        manager.notify_accounts(transaction);
                    }
                    Some(GeyserEvent::Block(block)) => manager.notify_block(block),
                    None => break,
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use parking_lot::Mutex as StdMutex;

    use agave_geyser_plugin_interface::geyser_plugin_interface::{
        GeyserPlugin, ReplicaAccountInfoVersions, ReplicaBlockInfoVersions,
        ReplicaTransactionInfoVersions, Result as PluginResult, SlotStatus,
    };
    use engine::testkit::TestEngine;
    use keeper::testkit::store_v42;
    use solana_account::AccountMode;
    use tokio_util::sync::CancellationToken;
    use v42_calculator_interface::builder::Expr as E;

    use super::{GeyserPluginManager, prepare_manager};
    use tokio::task::JoinSet;

    #[derive(Debug)]
    struct TransactionEvent {
        slot: u64,
        success: bool,
        logs: bool,
        cpi: bool,
        return_data: bool,
        compute_units: u64,
    }

    #[derive(Debug, Default)]
    struct Events {
        transactions: Vec<TransactionEvent>,
        accounts: Vec<(u64, u64, bool)>,
        slots: Vec<u64>,
        blocks: Vec<(u64, u64, u64, String)>,
    }

    #[derive(Debug)]
    struct FakePlugin(Arc<StdMutex<Events>>);

    impl GeyserPlugin for FakePlugin {
        fn name(&self) -> &'static str {
            "fake"
        }

        fn transaction_notifications_enabled(&self) -> bool {
            true
        }

        fn notify_transaction(
            &self,
            transaction: ReplicaTransactionInfoVersions<'_>,
            slot: u64,
        ) -> PluginResult<()> {
            let ReplicaTransactionInfoVersions::V0_0_2(info) = transaction else {
                panic!("expected transaction info v2")
            };
            let meta = info.transaction_status_meta;
            self.0.lock().transactions.push(TransactionEvent {
                slot,
                success: meta.status.is_ok(),
                logs: meta.log_messages.as_ref().is_some_and(|logs| !logs.is_empty()),
                cpi: meta.inner_instructions.as_ref().is_some_and(|groups| !groups.is_empty()),
                return_data: meta.return_data.is_some(),
                compute_units: meta.compute_units_consumed.unwrap_or_default(),
            });
            Ok(())
        }

        fn update_account(
            &self,
            account: ReplicaAccountInfoVersions<'_>,
            slot: u64,
            _is_startup: bool,
        ) -> PluginResult<()> {
            let ReplicaAccountInfoVersions::V0_0_3(info) = account else {
                panic!("expected account info v3")
            };
            self.0.lock().accounts.push((slot, info.write_version, info.txn.is_none()));
            Ok(())
        }

        fn update_slot_status(
            &self,
            slot: u64,
            _parent: Option<u64>,
            status: &SlotStatus,
        ) -> PluginResult<()> {
            assert_eq!(status, &SlotStatus::Rooted);
            self.0.lock().slots.push(slot);
            Ok(())
        }

        fn notify_block_metadata(&self, block: ReplicaBlockInfoVersions<'_>) -> PluginResult<()> {
            let ReplicaBlockInfoVersions::V0_0_4(info) = block else {
                panic!("expected block info v4")
            };
            self.0.lock().blocks.push((
                info.slot,
                info.executed_transaction_count,
                info.entry_count,
                info.parent_blockhash.to_owned(),
            ));
            Ok(())
        }
    }

    /// Proves that both plugins receive metadata and monotonically versioned writes.
    #[tokio::test(flavor = "multi_thread")]
    async fn fake_plugin_receives_engine_metadata_and_monotonic_writes() {
        let mut te = TestEngine::new().await;
        let output = store_v42(&te, 0, AccountMode::Magic);
        let events = Arc::new(StdMutex::new(Events::default()));
        let other_events = Arc::new(StdMutex::new(Events::default()));
        let manager = GeyserPluginManager::from_plugins(vec![
            Box::new(FakePlugin(events.clone())),
            Box::new(FakePlugin(other_events.clone())),
        ]);
        let cancel = CancellationToken::new();
        let mut tasks = JoinSet::new();
        prepare_manager(manager, (*te).clone())
            .unwrap()
            .unwrap()
            .spawn(&mut tasks, cancel.clone());

        te.execute(&[E::lit(7).cpi().compose(output, &[])])
            .await
            .expect("transaction succeeds");
        te.execute(&[E::lit(8).compose(output, &[])])
            .await
            .expect("second transaction succeeds");
        te.advance(1).await;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let ready = [&events, &other_events].iter().all(|events| {
                let events = events.lock();
                events.transactions.len() >= 2
                    && events.accounts.len() >= 2
                    && !events.slots.is_empty()
                    && !events.blocks.is_empty()
            });
            if ready {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                let events = events.lock();
                panic!(
                    "Geyser delivery timed out: transactions={}, accounts={}, slots={}, blocks={}",
                    events.transactions.len(),
                    events.accounts.len(),
                    events.slots.len(),
                    events.blocks.len(),
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        for events in [&events, &other_events] {
            let events = events.lock();
            let transaction = events.transactions.first().expect("transaction delivered");
            assert!(transaction.slot > 0);
            assert!(transaction.success, "successful result is retained");
            assert!(transaction.logs, "logs are retained");
            assert!(transaction.cpi, "CPI metadata is retained");
            assert!(transaction.return_data, "return data is retained");
            assert!(transaction.compute_units > 0, "compute units are retained");
            assert!(!events.accounts.is_empty(), "account updates are delivered");
            for versions in events.accounts.windows(2) {
                assert_eq!(versions[1].1, versions[0].1 + 1);
            }
            assert!(events.accounts.iter().all(|account| account.2));
            assert_eq!(events.slots.len(), 1, "slot update is delivered");
            let block = events.blocks.first().expect("block update delivered");
            assert_eq!((block.1, block.2), (0, 0));
            assert_eq!(
                block.3,
                te.blocks().latest().parent.to_string(),
                "parent_blockhash matches engine Block.parent"
            );
        }
        cancel.cancel();
        while let Some(task) = tasks.join_next().await {
            task.expect("Geyser task exits cleanly");
        }
        te.close().await;
    }
}
