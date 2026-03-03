use std::sync::Arc;

use magicblock_ledger::Ledger;

pub struct MagicSysAdapter {
    pub ledger: Arc<Ledger>,
}
