use std::{num::NonZeroUsize, sync::Arc};

use async_trait::async_trait;
use magicblock_accounts_db::AccountsDb;
use magicblock_program::outbox_intent_bundles::OutboxIntentBundle;
use solana_account::ReadableAccount;
use solana_pubkey::Pubkey;
// TODO(edwin): name - OutboxIntentAccount/PendingIntentAccount

// TODO(edwin): name - PendingIntentBundlesReader?
#[async_trait]
pub trait OutboxIntentBundlesReader: Send + 'static {
    type Error: Send;

    /// Reads `n` outbox intents
    /// Returns `OutboxIntentBundle` in ascending order by `ScheduledIntentBundle::id`
    /// If Vec::len != n, that means that there's no more Outbox intents
    async fn read(
        &mut self,
        n: NonZeroUsize,
    ) -> Result<Vec<OutboxIntentBundle>, Self::Error>;
}

pub struct InternalOutboxIntentBundlesReader {
    /// Current intent id pos
    intent_id_pos: Option<u64>,
    /// Accounts DB where we read intents from
    /// Could be an RpcClient in the future
    accounts_db: Arc<AccountsDb>,
}

impl InternalOutboxIntentBundlesReader {
    const TARGET_PROGRAM_ID: Pubkey = magicblock_program::ID;
    const ACCOUNT_DISCRIMINATOR: [u8; 8] = todo!();

    pub fn new(accounts_db: Arc<AccountsDb>) -> Self {
        Self {
            // Uninitialized state
            intent_id_pos: None,
            accounts_db,
        }
    }

    pub fn initialize_intent_pos(
        &mut self,
    ) -> Result<(), InternalOutboxIntentBundlesReader> {
        let asd = self
            .accounts_db
            .get_program_accounts(&Self::TARGET_PROGRAM_ID, |account| {
                if !account.data().starts_with(&Self::ACCOUNT_DISCRIMINATOR) {
                    return false;
                }

                true
            })
            .unwrap();
        todo!()
    }
}

#[async_trait]
impl OutboxIntentBundlesReader for InternalOutboxIntentBundlesReader {
    type Error = InternalOutboxIntentBundlesReaderError;

    async fn read(
        &mut self,
        n: NonZeroUsize,
    ) -> Result<Vec<OutboxIntentBundle>, Self::Error> {
        todo!()
    }
}

#[derive(thiserror::Error, Debug)]
pub enum InternalOutboxIntentBundlesReaderError {}
