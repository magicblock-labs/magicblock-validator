use std::{
    ops::{Deref, DerefMut},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    thread,
};

pub use guinea;
use log::error;
use magicblock_accounts_db::AccountsDb;
use magicblock_core::{
    link::{
        blocks::{BlockMeta, BlockUpdate, BlockUpdateTx},
        link,
        transactions::{
            SanitizeableTransaction, TransactionResult,
            TransactionSchedulerHandle, TransactionSimulationResult,
        },
        DispatchEndpoints,
    },
    traits::AccountsBank,
    Slot,
};
use magicblock_ledger::Ledger;
use magicblock_processor::{
    build_svm_env,
    scheduler::{state::TransactionSchedulerState, TransactionScheduler},
};
use solana_account::AccountSharedData;
pub use solana_instruction::*;
use solana_keypair::Keypair;
use solana_program::{
    hash::Hasher, native_token::LAMPORTS_PER_SOL, pubkey::Pubkey,
};
use solana_signature::Signature;
pub use solana_signer::Signer;
use solana_transaction::Transaction;
use solana_transaction_status_client_types::TransactionStatusMeta;
use tempfile::TempDir;

/// A simulated validator backend for integration tests.
///
/// This struct encapsulates all the core components of a validator, including
/// the `AccountsDb`, a `Ledger`, and a running `TransactionScheduler` with its
/// worker pool. It provides a high-level API for tests to manipulate the blockchain
/// state and process transactions.
pub struct ExecutionTestEnv {
    /// Atomic counter to index the payers array
    payer_index: AtomicUsize,
    /// The default keypair used for paying transaction fees and signing.
    pub payers: Vec<Keypair>,
    /// A handle to the accounts database, storing all account states.
    pub accountsdb: Arc<AccountsDb>,
    /// A handle to the ledger, storing all blocks and transactions.
    pub ledger: Arc<Ledger>,
    /// The entry point for submitting transactions to the processing pipeline.
    pub transaction_scheduler: TransactionSchedulerHandle,
    /// The temporary directory holding the `AccountsDb` and `Ledger` files for this test run.
    pub dir: TempDir,
    /// The "client-side" channel endpoints for listening to validator events.
    pub dispatch: DispatchEndpoints,
    /// The "server-side" channel endpoint for broadcasting new block updates.
    pub blocks_tx: BlockUpdateTx,
    /// Transaction execution scheduler/backend for deferred launch
    pub scheduler: Option<TransactionScheduler>,
}

impl Default for ExecutionTestEnv {
    fn default() -> Self {
        Self::new()
    }
}

impl ExecutionTestEnv {
    pub const BASE_FEE: u64 = 1000;

    /// Creates a new, fully initialized execution test environment.
    ///
    /// This function sets up a complete validator stack:
    /// 1.  Creates temporary on-disk storage for the accounts database and ledger.
    /// 2.  Initializes all the communication channels between the API layer and the core.
    /// 3.  Spawns a `TransactionScheduler` with one worker thread.
    /// 4.  Pre-loads a test program (`guinea`) for use in tests.
    /// 5.  Funds a default `payer` keypair with 1 SOL.
    pub fn new() -> Self {
        Self::new_with_config(Self::BASE_FEE, 1, false)
    }

    /// Creates a new, fully initialized validator test environment with given base fee
    ///
    /// This function sets up a complete validator stack:
    /// 1.  Creates temporary on-disk storage for the accounts database and ledger.
    /// 2.  Initializes all the communication channels between the API layer and the core.
    /// 3.  Spawns a `TransactionScheduler` with the configured number of worker threads.
    /// 4.  Pre-loads a test program (`guinea`) for use in tests.
    /// 5.  Funds a default `payer` keypair with 1 SOL.
    pub fn new_with_config(
        fee: u64,
        executors: u32,
        defer_startup: bool,
    ) -> Self {
        init_logger!();
        let dir =
            tempfile::tempdir().expect("creating temp dir for validator state");
        let accountsdb = Arc::new(
            AccountsDb::open(dir.path()).expect("opening test accountsdb"),
        );
        let ledger =
            Arc::new(Ledger::open(dir.path()).expect("opening test ledger"));

        let (dispatch, validator_channels) = link();
        let blockhash = ledger.latest_block().load().blockhash;
        let environment = build_svm_env(&accountsdb, blockhash, fee);
        let payers = (0..executors).map(|_| Keypair::new()).collect();

        let mut this = Self {
            payer_index: AtomicUsize::new(0),
            payers,
            accountsdb: accountsdb.clone(),
            ledger: ledger.clone(),
            transaction_scheduler: dispatch.transaction_scheduler.clone(),
            dir,
            dispatch,
            blocks_tx: validator_channels.block_update,
            scheduler: None,
        };
        this.advance_slot(); // Move to slot 1 to ensure a non-genesis state.

        let scheduler_state = TransactionSchedulerState {
            accountsdb,
            ledger,
            account_update_tx: validator_channels.account_update,
            transaction_status_tx: validator_channels.transaction_status,
            txn_to_process_rx: validator_channels.transaction_to_process,
            tasks_tx: validator_channels.tasks_service,
            environment,
            is_auto_airdrop_lamports_enabled: false,
        };

        // Load test program
        scheduler_state
            .load_upgradeable_programs(&[(
                guinea::ID,
                "../programs/elfs/guinea.so".into(),
            )])
            .expect("failed to load test programs into test env");

        // Start/Defer the transaction processing backend.
        let scheduler = TransactionScheduler::new(executors, scheduler_state);
        if defer_startup {
            this.scheduler.replace(scheduler);
        } else {
            scheduler.spawn();
        }

        for payer in this.payers.iter() {
            this.fund_account(payer.pubkey(), LAMPORTS_PER_SOL);
        }
        this
    }

    pub fn run_scheduler(&mut self) {
        if let Some(scheduler) = self.scheduler.take() {
            scheduler.spawn();
        }
    }

    /// Creates a new account with the specified properties.
    /// Note: This helper automatically marks the account as `delegated`.
    pub fn create_account_with_config(
        &self,
        lamports: u64,
        space: usize,
        owner: Pubkey,
    ) -> Keypair {
        let keypair = Keypair::new();
        let mut account = AccountSharedData::new(lamports, space, &owner);
        account.set_delegated(true);
        self.accountsdb.insert_account(&keypair.pubkey(), &account);
        keypair
    }

    /// Creates a new, empty system account with the given lamports.
    pub fn create_account(&self, lamports: u64) -> Keypair {
        self.create_account_with_config(lamports, 0, Default::default())
    }

    /// Funds an existing account with the given lamports.
    /// If the account does not exist, it will be created as a system account.
    pub fn fund_account(&self, pubkey: Pubkey, lamports: u64) {
        self.fund_account_with_owner(pubkey, lamports, Default::default());
    }

    /// Funds an account with a specific owner.
    /// Note: This helper automatically marks the account as `delegated`.
    pub fn fund_account_with_owner(
        &self,
        pubkey: Pubkey,
        lamports: u64,
        owner: Pubkey,
    ) {
        let mut account = AccountSharedData::new(lamports, 0, &owner);
        account.set_delegated(true);
        self.accountsdb.insert_account(&pubkey, &account);
    }

    /// Retrieves a transaction's metadata from the ledger by its signature.
    pub fn get_transaction(
        &self,
        sig: Signature,
    ) -> Option<TransactionStatusMeta> {
        self.ledger
            .get_transaction_status(sig, u64::MAX)
            .expect("failed to get transaction meta from ledger")
            .map(|(_, m)| m)
    }

    /// Simulates the production of a new block.
    ///
    /// This advances the slot, calculates a new blockhash, writes the block to the
    /// ledger, and broadcasts a `BlockUpdate` notification.
    pub fn advance_slot(&self) -> Slot {
        let block = self.ledger.latest_block();
        let b = block.load();
        let slot = b.slot + 1;
        let hash = {
            let mut hasher = Hasher::default();
            hasher.hash(b.blockhash.as_ref());
            hasher.hash(&b.slot.to_le_bytes());
            hasher.result()
        };
        let time = slot as i64;
        self.ledger
            .write_block(slot, time, hash)
            .expect("failed to write new block to the ledger");
        self.accountsdb.set_slot(slot);

        // Notify the system that a new block was produced.
        let _ = self.blocks_tx.send(BlockUpdate {
            hash,
            meta: BlockMeta { slot, time },
        });

        // Yield to allow other tasks (like the executor) to process the slot change.
        thread::yield_now();
        slot
    }

    /// Builds a transaction with the given instructions, signed by the default payer.
    pub fn build_transaction(&self, ixs: &[Instruction]) -> Transaction {
        let payer = {
            let index = self.payer_index.fetch_add(1, Ordering::Relaxed);
            &self.payers[index % self.payers.len()]
        };
        Transaction::new_signed_with_payer(
            ixs,
            Some(&payer.pubkey()),
            &[payer],
            self.ledger.latest_blockhash(),
        )
    }

    /// Submits a transaction for execution and waits for its result.
    pub async fn execute_transaction(
        &self,
        txn: impl SanitizeableTransaction,
    ) -> TransactionResult {
        self.transaction_scheduler
            .execute(txn)
            .await
            .inspect_err(|err| error!("failed to execute transaction: {err}"))
    }

    /// Submits a transaction for scheduling and returns
    pub async fn schedule_transaction(
        &self,
        txn: impl SanitizeableTransaction,
    ) {
        self.transaction_scheduler.schedule(txn).await.unwrap();
    }

    /// Submits a transaction for simulation and waits for the detailed result.
    pub async fn simulate_transaction(
        &self,
        txn: impl SanitizeableTransaction,
    ) -> TransactionSimulationResult {
        let result = self
            .transaction_scheduler
            .simulate(txn)
            .await
            .expect("transaction executor has shutdown during test");
        if let Err(ref err) = result.result {
            error!("failed to simulate transaction: {err}")
        }
        result
    }

    /// Submits a transaction for replay and waits for its result.
    pub async fn replay_transaction(
        &self,
        txn: impl SanitizeableTransaction,
    ) -> TransactionResult {
        self.transaction_scheduler
            .replay(txn)
            .await
            .inspect_err(|err| error!("failed to replay transaction: {err}"))
    }

    pub fn get_account(&self, pubkey: Pubkey) -> CommitableAccount<'_> {
        let account = self
            .accountsdb
            .get_account(&pubkey)
            .expect("only existing accounts should be requested during tests");
        CommitableAccount {
            pubkey,
            account,
            db: &self.accountsdb,
        }
    }

    pub fn get_payer(&self) -> CommitableAccount<'_> {
        let payer = {
            let index = self.payer_index.load(Ordering::Relaxed);
            &self.payers[index % self.payers.len()]
        };
        self.get_account(payer.pubkey())
    }
}

pub struct CommitableAccount<'db> {
    pub pubkey: Pubkey,
    pub account: AccountSharedData,
    pub db: &'db AccountsDb,
}

impl CommitableAccount<'_> {
    pub fn commmit(self) {
        self.db.insert_account(&self.pubkey, &self.account);
    }
}

impl Deref for CommitableAccount<'_> {
    type Target = AccountSharedData;
    fn deref(&self) -> &Self::Target {
        &self.account
    }
}

impl DerefMut for CommitableAccount<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.account
    }
}

pub mod macros;
