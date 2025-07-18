use std::{fs, path::Path};

use magicblock_accounts_db::ACCOUNTSDB_DIR;

pub fn remove_ledger_directory_if_exists(
    dir: &Path,
    keep_accounts_db: bool,
) -> Result<(), std::io::Error> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;

        if entry.file_name() == ACCOUNTSDB_DIR && keep_accounts_db {
            continue;
        }

        if entry.metadata()?.is_dir() {
            fs::remove_dir_all(entry.path())?
        } else {
            fs::remove_file(entry.path())?
        }
    }
    Ok(())
}
