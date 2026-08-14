use std::{
    cmp::Ordering,
    collections::{BinaryHeap, VecDeque},
    num::NonZeroUsize,
    sync::Arc,
};

use async_trait::async_trait;
use magicblock_accounts_db::{
    error::AccountsDbError, traits::AccountsBank, AccountsDb,
};
use magicblock_core::intent::outbox::{
    outbox_intent_pda, OUTBOX_INTENT_DISCRIMINATOR,
};
use magicblock_program::outbox_intent_bundles::OutboxIntentBundle;
use solana_account::{AccountSharedData, ReadableAccount};
use tracing::warn;

#[async_trait]
pub trait OutboxIntentBundlesReader: Send + 'static {
    type Error: Send;

    /// Returns up to `n` outbox intents sorted ascending by `ScheduledIntentBundle::id`.
    /// If `Vec::len() < n`, no more intents are available.
    async fn read(
        &mut self,
        n: usize,
    ) -> Result<Vec<OutboxIntentBundle>, Self::Error>;

    /// Fetches a single outbox intent by id. Returns `None` if the account does not exist.
    async fn fetch_outbox_intent(
        &self,
        intent_id: u64,
    ) -> Result<Option<OutboxIntentBundle>, Self::Error>;
}

pub struct InternalOutboxIntentBundlesReader {
    /// Capacity of the buffer
    capacity: NonZeroUsize,
    /// Sorted ascending by intent id, drained from front on read()
    buffer: VecDeque<OutboxIntentBundle>,
    /// Max id returned so far; refill skips ids <= this
    last_consumed_id: Option<u64>,
    /// Accounts DB where we read intents from
    /// Could be an RpcClient in the future
    accounts_db: Arc<AccountsDb>,
}

impl InternalOutboxIntentBundlesReader {
    pub fn new(accounts_db: Arc<AccountsDb>, capacity: NonZeroUsize) -> Self {
        Self {
            capacity,
            buffer: VecDeque::new(),
            last_consumed_id: None,
            accounts_db,
        }
    }

    // Refills buffer with OutboxIntents up to capacity
    fn refill(&mut self) -> Result<(), AccountsDbError> {
        #[repr(transparent)]
        struct OrderedIntent {
            inner: OutboxIntentBundle,
        }
        impl Eq for OrderedIntent {}
        impl PartialEq for OrderedIntent {
            fn eq(&self, other: &Self) -> bool {
                self.inner.id.eq(&other.inner.id)
            }
        }
        impl PartialOrd for OrderedIntent {
            fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
                Some(self.cmp(other))
            }
        }
        impl Ord for OrderedIntent {
            fn cmp(&self, other: &Self) -> Ordering {
                self.inner.id.cmp(&other.inner.id)
            }
        }

        let outbox_candidates_iter = self.accounts_db.get_program_accounts(
            &magicblock_program::ID,
            Self::outbox_accounts_filter,
        )?;

        // Create iterator that yields valid, unconsumed intents
        let outbox_iter = outbox_candidates_iter
            .map(|(address, account)| {
                (address, OutboxIntentBundle::try_from_bytes(account.data()))
            })
            // Filter out failed deserializations + warn
            .filter_map(|(pubkey, account_result)| match account_result {
                Ok(value) => Some(value),
                Err(err) => {
                    warn!(error = ?err, "Account with OutboxIntentBundle failed to deserialize, pubkey; {}", pubkey);
                    None
                }
            })
            // Filter out already consumed intents
            .filter(|outbox_intent| if let Some(last_consumed_id) = self.last_consumed_id {
                outbox_intent.id > last_consumed_id
            } else {
                true
            });

        // Retain only `capacity` of smallest id's intents
        let capacity = self.capacity.get();
        let mut heap: BinaryHeap<OrderedIntent> =
            BinaryHeap::with_capacity(capacity);
        for outbox_intent_bundle in outbox_iter {
            heap.push(OrderedIntent {
                inner: outbox_intent_bundle,
            });
            if heap.len() > capacity {
                heap.pop();
            }
        }

        // SAFETY: OrderedIntent is #[repr(transparent)] over OutboxIntentBundle —
        // identical memory layout, zero-copy cast
        let mut items: Vec<OutboxIntentBundle> = unsafe {
            let mut v = std::mem::ManuallyDrop::new(heap.into_vec());
            Vec::from_raw_parts(
                v.as_mut_ptr() as *mut OutboxIntentBundle,
                v.len(),
                v.capacity(),
            )
        };
        items.sort_unstable_by_key(|b| b.id);
        self.last_consumed_id =
            items.last().map(|b| b.id).or(self.last_consumed_id);
        self.buffer.extend(items);
        Ok(())
    }

    /// Filter for OutboxIntentBundle accounts, those start with OUTBOX_INTENT_DISCRIMINATOR
    fn outbox_accounts_filter(account: &AccountSharedData) -> bool {
        account.data().starts_with(&OUTBOX_INTENT_DISCRIMINATOR)
    }
}

#[async_trait]
impl OutboxIntentBundlesReader for InternalOutboxIntentBundlesReader {
    type Error = OutboxIntentBundlesReaderError;

    /// Returns up to `n` outbox intents sorted ascending by `ScheduledIntentBundle::id`.
    /// If `Vec::len() < n`, no more intents are available.
    ///
    /// When the internal buffer runs low, triggers a full `getProgramAccounts` scan
    /// to refill — O(N log C) where N is open intent count and C is capacity.
    /// Callers recommended to drive each read batch to completion and close the accounts
    /// before reading again; closed accounts are removed from the DB and won't
    /// be scanned on the next refill.
    async fn read(
        &mut self,
        n: usize,
    ) -> Result<Vec<OutboxIntentBundle>, Self::Error> {
        if n == 0 {
            return Ok(vec![]);
        }
        let capacity = self.capacity.get();
        if n > capacity {
            return Err(Self::Error::ReadExceedsCapacityError(n, capacity));
        }

        // Refill if buffer runs low
        if self.buffer.len() < n {
            self.refill()?;
        }

        // Take available amount
        let take = n.min(self.buffer.len());
        Ok(self.buffer.drain(..take).collect())
    }

    async fn fetch_outbox_intent(
        &self,
        intent_id: u64,
    ) -> Result<Option<OutboxIntentBundle>, Self::Error> {
        let pda = outbox_intent_pda(intent_id);
        let Some(account) = self.accounts_db.get_account(&pda) else {
            return Ok(None);
        };
        Ok(Some(OutboxIntentBundle::try_from_bytes(account.data())?))
    }
}

#[derive(thiserror::Error, Debug)]
pub enum OutboxIntentBundlesReaderError {
    #[error("Requested read exceeded capacity, n: {0}, capacity: {1}")]
    ReadExceedsCapacityError(usize, usize),
    #[error("AccountsDbError: {0}")]
    AccountsDbError(#[from] AccountsDbError),
    #[error("BincodeError: {0}")]
    BincodeError(#[from] bincode::Error),
}

pub type OutboxIntentBundlesReaderResult<T> =
    Result<T, OutboxIntentBundlesReaderError>;

#[cfg(test)]
mod tests {
    use std::{num::NonZeroUsize, sync::Arc};

    use magicblock_accounts_db::AccountsDb;
    use magicblock_core::intent::outbox::outbox_intent_pda;
    use magicblock_program::{
        magic_scheduled_base_intent::{
            MagicIntentBundle, ScheduledIntentBundle,
        },
        outbox_intent_bundles::OutboxIntentBundle,
    };
    use solana_account::{AccountSharedData, WritableAccount};
    use solana_hash::Hash;
    use solana_pubkey::Pubkey;
    use solana_transaction::Transaction;

    use super::{InternalOutboxIntentBundlesReader, OutboxIntentBundlesReader};

    fn make_db() -> (Arc<AccountsDb>, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("temp dir");
        let db = AccountsDb::open(dir.path()).expect("db init").into();
        (db, dir)
    }

    fn make_bundle(id: u64) -> OutboxIntentBundle {
        let inner = ScheduledIntentBundle {
            id,
            slot: 0,
            blockhash: Hash::default(),
            sent_transaction: Transaction::default(),
            payer: Pubkey::default(),
            intent_bundle: MagicIntentBundle::default(),
        };
        OutboxIntentBundle::accepted(inner)
    }

    fn insert_bundle(db: &AccountsDb, bundle: &OutboxIntentBundle) {
        let bytes = bundle.try_to_bytes().expect("serialize");
        let pubkey = outbox_intent_pda(bundle.inner.id);
        let mut account =
            AccountSharedData::new(1, bytes.len(), &magicblock_program::ID);
        account.data_as_mut_slice().copy_from_slice(&bytes);
        db.insert_account(&pubkey, &account).expect("insert");
    }

    #[tokio::test]
    async fn read_returns_ascending_order() {
        let (db, _dir) = make_db();
        insert_bundle(&db, &make_bundle(9));
        insert_bundle(&db, &make_bundle(1));
        insert_bundle(&db, &make_bundle(4));

        let mut reader = InternalOutboxIntentBundlesReader::new(
            db,
            NonZeroUsize::new(10).unwrap(),
        );
        let result = reader.read(3).await.unwrap();
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].inner.id, 1);
        assert_eq!(result[1].inner.id, 4);
        assert_eq!(result[2].inner.id, 9);
    }

    #[tokio::test]
    async fn read_fewer_than_n_when_db_has_less() {
        let (db, _dir) = make_db();
        insert_bundle(&db, &make_bundle(3));

        let mut reader = InternalOutboxIntentBundlesReader::new(
            db,
            NonZeroUsize::new(10).unwrap(),
        );
        let result = reader.read(5).await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].inner.id, 3);
    }

    #[tokio::test]
    async fn read_respects_capacity_smallest() {
        let (db, _dir) = make_db();
        for id in [10, 5, 1, 8, 3] {
            insert_bundle(&db, &make_bundle(id));
        }

        let mut reader = InternalOutboxIntentBundlesReader::new(
            db,
            NonZeroUsize::new(3).unwrap(),
        );
        let result = reader.read(3).await.unwrap();
        assert_eq!(
            result.iter().map(|b| b.inner.id).collect::<Vec<_>>(),
            vec![1, 3, 5]
        );
    }

    #[tokio::test]
    async fn read_exceeds_capacity_errors() {
        let (db, _dir) = make_db();
        let mut reader = InternalOutboxIntentBundlesReader::new(
            db,
            NonZeroUsize::new(3).unwrap(),
        );
        assert!(reader.read(4).await.is_err());
    }

    #[tokio::test]
    async fn sequential_reads_dont_repeat() {
        let (db, _dir) = make_db();
        for id in 1..=6 {
            insert_bundle(&db, &make_bundle(id));
        }

        let mut reader = InternalOutboxIntentBundlesReader::new(
            db,
            NonZeroUsize::new(5).unwrap(),
        );
        let first = reader.read(3).await.unwrap();
        let second = reader.read(3).await.unwrap();

        let first_ids: Vec<_> = first.iter().map(|b| b.inner.id).collect();
        let second_ids: Vec<_> = second.iter().map(|b| b.inner.id).collect();
        assert_eq!(first_ids, vec![1, 2, 3]);
        assert_eq!(second_ids, vec![4, 5, 6]);
    }
}
