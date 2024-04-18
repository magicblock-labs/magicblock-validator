use std::{fs, path::Path};

use rocksdb::DB;

use super::{
    cf_descriptors::cf_descriptors,
    errors::BlockstoreResult,
    options::{AccessType, BlockstoreOptions},
    rocksdb_options::get_rocksdb_options,
};

// -----------------
// Rocks
// -----------------
#[derive(Debug)]
struct Rocks {
    db: rocksdb::DB,
    access_type: AccessType,
}

impl Rocks {
    pub fn open(
        path: &Path,
        options: BlockstoreOptions,
    ) -> BlockstoreResult<Self> {
        let access_type = options.access_type.clone();
        fs::create_dir_all(path)?;

        let db_options = get_rocksdb_options(&access_type);
        let descriptors = cf_descriptors(path, &options);

        let db = match access_type {
            AccessType::Primary => {
                DB::open_cf_descriptors(&db_options, path, descriptors)?
            }
            _ => unreachable!("Only primary access is supported"),
        };

        // TODO(thlorenz): @@@rocksdb do we need to configure compaction?

        Ok(Self { db, access_type })
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*, crate::blockstore::columns::columns, rocksdb::Options,
        std::path::PathBuf, tempfile::tempdir,
    };

    #[test]
    fn test_cf_names_and_descriptors_equal_length() {
        let path = PathBuf::default();
        let options = BlockstoreOptions::default();
        // The names and descriptors don't need to be in the same order for our use cases;
        // however, there should be the same number of each. For example, adding a new column
        // should update both lists.
        assert_eq!(columns().len(), cf_descriptors(&path, &options,).len());
    }

    #[test]
    fn test_open_unknown_columns() {
        let temp_dir = tempdir().unwrap();
        let db_path = temp_dir.path();

        // Open with Primary to create the new database
        {
            let options = BlockstoreOptions {
                access_type: AccessType::Primary,
            };
            let mut rocks = Rocks::open(db_path, options).unwrap();

            // Introduce a new column that will not be known
            rocks
                .db
                .create_cf("new_column", &Options::default())
                .unwrap();
        }

        // Opening with either Secondary or Primary access should succeed,
        // even though the Rocks code is unaware of "new_column"
        {
            let options = BlockstoreOptions {
                access_type: AccessType::Primary,
            };
            let _ = Rocks::open(db_path, options).unwrap();
        }
    }
}
