use std::{path::PathBuf, sync::Arc};

use crate::database::db::Database;

pub struct Blockstore {
    ledger_path: PathBuf,
    db: Arc<Database>,
}

impl Blockstore {}
