use {
    crate::geyser_plugin_manager::GeyserPluginManager,
    log::*,
    sleipnir_bank::slot_status_notifier_interface::SlotStatusNotifier,
    solana_geyser_plugin_interface::geyser_plugin_interface::SlotStatus,
    solana_measure::measure::Measure,
    solana_metrics::*,
    solana_sdk::clock::Slot,
    std::sync::{Arc, RwLock},
};

#[derive(Debug)]
pub struct SlotStatusNotifierImpl {
    plugin_manager: Arc<RwLock<GeyserPluginManager>>,
}

impl SlotStatusNotifierImpl {
    pub fn new(plugin_manager: Arc<RwLock<GeyserPluginManager>>) -> Self {
        Self { plugin_manager }
    }
}

impl SlotStatusNotifier for SlotStatusNotifierImpl {
    fn notify_slot_status(&self, slot: Slot, parent: Option<Slot>) {
        // We use a single bank only
        let slot_status: SlotStatus = SlotStatus::Processed;

        let plugin_manager = self.plugin_manager.read().unwrap();
        if plugin_manager.plugins.is_empty() {
            return;
        }

        for plugin in plugin_manager.plugins.iter() {
            let mut measure = Measure::start("geyser-plugin-update-slot");
            match plugin.update_slot_status(slot, parent, slot_status) {
                Err(err) => {
                    error!(
                        "Failed to update slot status at slot {}, error: {} to plugin {}",
                        slot,
                        err,
                        plugin.name()
                    )
                }
                Ok(_) => {
                    trace!(
                        "Successfully updated slot status at slot {} to plugin {}",
                        slot,
                        plugin.name()
                    );
                }
            }
            measure.stop();
            inc_new_counter_debug!(
                "geyser-plugin-update-slot-us",
                measure.as_us() as usize,
                1000,
                1000
            );
        }
    }
}
