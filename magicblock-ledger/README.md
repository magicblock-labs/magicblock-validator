
# Summary

Stores all types of chain information in key-value stores: signatures, statuses, metas, slots.
This is basically a massive optimized and serialized `HashMap<Column, OrderedHashMap<Key, Value>>`.
Uses rocksdb library internally as a fancy storage datastructure that automatically saves to file.

# Details

*Important symbols:*

- `Ledger` struct
  - Depends on a `Database`
  - Contains a bunch of `LedgeColumn`, one for each stored data type
  - Implements all the fetching/putting/serialization logic for each stored data type

- `Database` struct
  - Depends on a `Rocks` which depends on `rocksdb::DB`
  - Just a fast column (namespace) and key-value (ordered-hash-map) database
  - Allows fetching generic deserialized datastructure directly

- `LedgerColumn` struct
  - Represent a single key-value store (or namespace) in the rocksdb
  - Expose get/put/iter/delete (with optionally protobuf) rocksdb's methods

# Notes

N/A
