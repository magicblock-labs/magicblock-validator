use std::{
    cmp::Ordering,
    collections::{BinaryHeap, VecDeque},
    num::NonZeroUsize,
    sync::Arc,
};

use async_trait::async_trait;
use magicblock_accounts_db::{error::AccountsDbError, AccountsDb};
use magicblock_core::intent::outbox::OUTBOX_INTENT_DISCRIMINATOR;
use magicblock_program::outbox_intent_bundles::OutboxIntentBundle;
use solana_account::{AccountSharedData, ReadableAccount};
use solana_pubkey::Pubkey;
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
    const TARGET_PROGRAM_ID: Pubkey = magicblock_program::ID;

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
        if n > self.capacity.get() {
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
}

#[derive(thiserror::Error, Debug)]
pub enum OutboxIntentBundlesReaderError {
    #[error("Requested read exceeded capacity, n: {0}, capacity: {1}")]
    ReadExceedsCapacityError(usize, usize),
    #[error("AccountsDbError: {0}")]
    AccountsDbError(#[from] AccountsDbError),
}

pub type OutboxIntentBundlesReaderResult<T> =
    Result<T, OutboxIntentBundlesReaderError>;
