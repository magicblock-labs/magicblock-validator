# Accounts Database Manager

The AccountsDB is a crucial component of the Magicblock Validator. It handles
the storage, retrieval, and management of account data. It's highly optimized
to provide efficient account search and retrieval, using memory mapped storage,
while still being persisted to disk.

## Features

- **Account Storage**: Efficient storage of account records, with support for
  both owned and borrowed account data. By default accounts are read directly
  from memory mapping without any deserialization and/or allocation, as
  consequence it allows for direct memory modification in situ in the database,
  avoid read modify write overhead. But this imposes a strict requirement, that
  no two threads have access to the same account at the same time for
  modification, which is naturally achieved with account locking when executing
  transactions. But any other way to hold onto borrowed state of account is
  prohibitted, as it will invetably cause an undefined behavior.
- **Index Management**: Fast index-based lookups for account data.
  LMDB is used for the purposes of indexing, where several different mappings
  are maintained to support database integrity.
- **Snapshot Management**: Automated snapshot creation to enable rollbacks to
  previous states of the database if necessary.
- **Concurrency Control**: Uses a "Stop the World" lock to ensure data
  consistency during critical operations like snapshotting. This prevents any
  writes to the database while it's being manipulated at the OS level. 
- **Program Account Scanning**: Retrieve and filter accounts associated with a
  specific program, the scanning happens without deserializing anything from
  database.

## Main API methods

- **Initialization**: Use `AccountsDb::new` to create or open an accounts
  database instance.
- **Account Operations**: Use `get_account`, `insert_account`,
  `contains_account` to read, write, or check the existence of accounts.
- **Snapshot Operations**: Schedule and manage snapshots with `set_slot`, which
  might trigger snapshot operation, which in turn involves locking everyting
  down, thus snapshot frequency should be set to sane value. And retrieve
  snapshot slots using `get_latest_snapshot_slot` and
  `get_oldest_snapshot_slot`.
- **Database Integrity**: Use `ensure_at_most` to ensure database state up to a
  specific slot, with rollback to previously taken snapshot if needed.
