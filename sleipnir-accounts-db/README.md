## AccountsDB

### Persistence

#### AccountStorage

Provides mapping of `AccountStorageEntry`s by slot.

Exposed on the `AccountsDb` as `pub storage: AccountStorage` in Solana.
In our case we wrapped this inside the `AccountsPersister`.

It supports some _shrink_ process running which is why it maintains two maps.
When querying `map` is checked before `shrink_in_progress_map`.

A `store_id` needs to be provided when looking up an entry.
It is matched against the `id` of the `AccountStorageEntry` in either map to return
a valid match.

It seems like most of the complexity comes from the _shrink in process_ feature, otherwise
it could just a simple map.


```rs
pub struct AccountStorage {
    /// map from Slot -> the single append vec for the slot
    map: AccountStorageMap,
    /// while shrink is operating on a slot, there can be 2 append vecs active for that slot
    /// Once the index has been updated to only refer to the new append vec, the single entry for the slot in 'map' can be updated.
    /// Entries in 'shrink_in_progress_map' can be found by 'get_account_storage_entry'
    shrink_in_progress_map: DashMap<Slot, Arc<AccountStorageEntry>>,
}

pub type AccountStorageMap = DashMap<Slot, AccountStorageReference>;
pub struct AccountStorageReference {
    /// the single storage for a given slot
    pub storage: Arc<AccountStorageEntry>,
    /// id can be read from 'storage', but it is an atomic read.
    /// id will never change while a storage is held, so we store it separately here for faster runtime lookup in 'get_account_storage_entry'
    pub id: AppendVecId,
}
```

**Find an `AccountStorageEntry` by slot and store_id**

```rs
pub(crate) fn get_account_storage_entry(
    &self,
    slot: Slot,
    store_id: AppendVecId,
) -> Option<Arc<AccountStorageEntry>>
```

**Insert an `AccountStorageEntry` into the `AccountStorage`**

Only available while no _shrink_ is in process.

```rs
fn insert(&self, slot: Slot, store: Arc<AccountStorageEntry>)
```

Extends the `map` with the entry.

#### AccountStorageEntry

Persistent storage structure holding the accounts.

```rs
#[derive(Debug)]
pub struct AccountStorageEntry {
    pub(crate) id: AtomicAppendVecId,

    pub(crate) slot: AtomicU64,

    /// storage holding the accounts
    pub accounts: AccountsFile,

    /// Keeps track of the number of accounts stored in a specific AppendVec.
    /// This is periodically checked to reuse the stores that do not have
    /// any accounts in it
    /// Lets us know that the append_vec, once maxed out, then emptied,
    /// can be reclaimed
    count_and_status: SeqLock<(usize, AccountStorageStatus)>,

    /// This is the total number of accounts stored ever since initialized to keep
    /// track of lifetime count of all store operations. And this differs from
    /// count_and_status in that this field won't be decremented.
    ///
    /// This is used as a rough estimate for slot shrinking. As such a relaxed
    /// use case, this value ARE NOT strictly synchronized with count_and_status!
    approx_store_count: AtomicUsize,
    [ .. ]
}
```

#### AccountsFile/AppendVec

A thread-safe, file-backed block of memory used to store `Account` instances.
Append operations are serialized such that only one thread updates the internal
`append_lock` at a time.
No restrictions are placed on reading, i.e we can read items from one thread while another is
appending new items.


```rs
#[derive(Debug, AbiExample)]
pub struct AppendVec {
    /// The file path where the data is stored.
    path: PathBuf,

    /// A file-backed block of memory that is used to store the data for each appended item.
    map: MmapMut,

    /// A lock used to serialize append operations.
    append_lock: Mutex<()>,

    /// The number of bytes used to store items, not the number of items.
    current_len: AtomicUsize,

    /// The number of bytes available for storing items.
    file_size: u64,
}
```

Exposed as an enum variant of `AccountsFile` in `AccountStorageEntry`

```rs
pub enum AccountsFile {
    AppendVec(AppendVec),
}
```

Has a `fn flush(&self)` which flushes the underlying `MmapMut` to disk.
