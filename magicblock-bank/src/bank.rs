use std::{
    borrow::Cow,
    collections::HashSet,
    mem,
    path::PathBuf,
    slice,
    sync::{
        atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering},
        Arc, LockResult, RwLock, RwLockReadGuard, RwLockWriteGuard,
    },
    time::Duration,
};

use log::{debug, info, trace};
use magicblock_accounts_db::{
    accounts::Accounts, accounts_db::AccountsDb, errors::AccountsDbResult,
    geyser::AccountsUpdateNotifier,
};
use solana_accounts_db::{
    accounts_index::ScanConfig, blockhash_queue::BlockhashQueue,
};
use solana_bpf_loader_program::syscalls::{
    create_program_runtime_environment_v1,
    create_program_runtime_environment_v2,
};
use solana_cost_model::cost_tracker::CostTracker;
use solana_geyser_plugin_manager::slot_status_notifier::SlotStatusNotifierImpl;
use solana_measure::{measure::Measure, measure_us};
use solana_program_runtime::loaded_programs::{
    BlockRelation, ForkGraph, ProgramCacheEntry,
};
use solana_rpc::slot_status_notifier::SlotStatusNotifierInterface;
use solana_runtime_transaction::instructions_processor::process_compute_budget_instructions;
use solana_sdk::{
    account::{
        create_account_shared_data_with_fields as create_account, from_account,
        Account, AccountSharedData, InheritableAccountFields, ReadableAccount,
        WritableAccount,
    },
    account_utils::StateMut,
    clock::{
        Epoch, Slot, SlotIndex, UnixTimestamp, DEFAULT_MS_PER_SLOT,
        INITIAL_RENT_EPOCH, MAX_PROCESSING_AGE, MAX_RECENT_BLOCKHASHES,
    },
    epoch_info::EpochInfo,
    epoch_schedule::EpochSchedule,
    feature,
    feature_set::{self, FeatureSet},
    fee::{FeeBudgetLimits, FeeStructure},
    fee_calculator::FeeRateGovernor,
    genesis_config::GenesisConfig,
    hash::{Hash, Hasher},
    message::{AccountKeys, SanitizedMessage},
    native_loader,
    nonce::{self, state::DurableNonce, NONCED_TX_MARKER_IX_INDEX},
    nonce_account,
    packet::PACKET_DATA_SIZE,
    precompiles::get_precompiles,
    pubkey::Pubkey,
    rent_collector::RentCollector,
    rent_debits::RentDebits,
    signature::Signature,
    slot_hashes::SlotHashes,
    slot_history::{Check, SlotHistory},
    sysvar::{self, last_restart_slot::LastRestartSlot},
    transaction::{
        Result, SanitizedTransaction, TransactionError,
        TransactionVerificationMode, VersionedTransaction,
        MAX_TX_ACCOUNT_LOCKS,
    },
    transaction_context::TransactionAccount,
};
use solana_svm::{
    account_loader::{
        CheckedTransactionDetails, LoadedTransaction, TransactionCheckResult,
    },
    account_overrides::AccountOverrides,
    nonce_info::NonceInfo,
    runtime_config::RuntimeConfig,
    transaction_commit_result::{
        CommittedTransaction, TransactionCommitResult,
    },
    transaction_error_metrics::TransactionErrorMetrics,
    transaction_execution_result::TransactionLoadedAccountsStats,
    transaction_processing_callback::{
        AccountState, TransactionProcessingCallback,
    },
    transaction_processing_result::{
        ProcessedTransaction, TransactionProcessingResult,
        TransactionProcessingResultExtensions,
    },
    transaction_processor::{
        ExecutionRecordingConfig, TransactionBatchProcessor,
        TransactionProcessingConfig, TransactionProcessingEnvironment,
    },
};
use solana_svm_transaction::svm_message::SVMMessage;
use solana_timings::{ExecuteTimingType, ExecuteTimings};

use crate::{
    bank_helpers::{
        calculate_data_size_delta, get_epoch_secs,
        inherit_specially_retained_account_fields,
    },
    bank_rc::BankRc,
    builtins::{BuiltinPrototype, BUILTINS},
    status_cache::StatusCache,
    transaction_batch::TransactionBatch,
    transaction_logs::{
        TransactionLogCollector, TransactionLogCollectorConfig,
    },
    transaction_results::{
        LoadAndExecuteTransactionsOutput, ProcessedTransactionCounts,
        TransactionBalances, TransactionBalancesSet,
    },
    transaction_simulation::TransactionSimulationResult,
};

pub type BankStatusCache = StatusCache<Result<()>>;

pub struct CommitTransactionCounts {
    pub committed_transactions_count: u64,
    pub committed_non_vote_transactions_count: u64,
    pub committed_with_failure_result_count: u64,
    pub signature_count: u64,
}

// -----------------
// ForkGraph
// -----------------
#[derive(Default)]
pub struct SimpleForkGraph;

impl ForkGraph for SimpleForkGraph {
    /// Returns the BlockRelation of A to B
    fn relationship(&self, _a: Slot, _b: Slot) -> BlockRelation {
        BlockRelation::Unrelated
    }
}

// -----------------
// Bank
// -----------------
//#[derive(Debug)]
pub struct Bank {
    /// References to accounts, parent and signature status
    pub rc: BankRc,

    /// Bank slot (i.e. block)
    slot: AtomicU64,

    /// Bank epoch
    epoch: Epoch,

    /// Validator Identity
    identity_id: Pubkey,

    /// initialized from genesis
    pub(crate) epoch_schedule: EpochSchedule,

    /// Transaction fee structure
    pub fee_structure: FeeStructure,

    /// Optional config parameters that can override runtime behavior
    pub(crate) runtime_config: Arc<RuntimeConfig>,

    /// A boolean reflecting whether any entries were recorded into the PoH
    /// stream for the slot == self.slot
    is_delta: AtomicBool,

    pub(crate) transaction_processor:
        RwLock<TransactionBatchProcessor<SimpleForkGraph>>,

    fork_graph: Arc<RwLock<SimpleForkGraph>>,

    // Global configuration for how transaction logs should be collected across all banks
    pub transaction_log_collector_config:
        Arc<RwLock<TransactionLogCollectorConfig>>,

    // Logs from transactions that this Bank executed collected according to the criteria in
    // `transaction_log_collector_config`
    pub transaction_log_collector: Arc<RwLock<TransactionLogCollector>>,

    transaction_debug_keys: Option<Arc<HashSet<Pubkey>>>,

    /// A cache of signature statuses
    pub status_cache: Arc<RwLock<BankStatusCache>>,

    // First path provided to accounts db (in our case it is always one)
    pub accounts_path: PathBuf,

    // -----------------
    // Counters
    // -----------------
    /// The number of transactions processed without error
    transaction_count: AtomicU64,

    /// The number of non-vote transactions processed without error since the most recent boot from
    /// snapshot or genesis. This value is not shared though the network, nor retained within
    /// snapshots, but is preserved in `Bank::new_from_parent`.
    non_vote_transaction_count_since_restart: AtomicU64,

    /// The number of transaction errors in this slot
    transaction_error_count: AtomicU64,

    /// The number of transaction entries in this slot
    transaction_entries_count: AtomicU64,

    /// The max number of transaction in an entry in this slot
    transactions_per_entry_max: AtomicU64,

    /// The change to accounts data size in this Bank, due on-chain events (i.e. transactions)
    accounts_data_size_delta_on_chain: AtomicI64,

    /// The change to accounts data size in this Bank, due to off-chain events (i.e. when adding a program account)
    accounts_data_size_delta_off_chain: AtomicI64,

    /// The number of signatures from valid transactions in this slot
    signature_count: AtomicU64,

    // -----------------
    // Genesis related
    // -----------------
    /// Total capitalization, used to calculate inflation
    capitalization: AtomicU64,

    /// The initial accounts data size at the start of this Bank, before processing any transactions/etc
    pub(super) accounts_data_size_initial: u64,

    /// Track cluster signature throughput and adjust fee rate
    pub(crate) fee_rate_governor: FeeRateGovernor,
    //
    // Bank max_tick_height
    max_tick_height: u64,

    /// The number of hashes in each tick. None value means hashing is disabled.
    hashes_per_tick: Option<u64>,

    /// The number of ticks in each slot.
    ticks_per_slot: u64,

    /// length of a slot in ns which is provided via the genesis config
    /// NOTE: this is not currenlty configured correctly, use [Self::millis_per_slot] instead
    pub ns_per_slot: u128,

    /// genesis time, used for computed clock
    genesis_creation_time: UnixTimestamp,

    /// The number of slots per year, used for inflation
    /// which is provided via the genesis config
    /// NOTE: this is not currenlty configured correctly, use [Self::millis_per_slot] instead
    slots_per_year: f64,

    /// Milliseconds per slot which is provided directly when the bank is created
    pub millis_per_slot: u64,

    // The number of block/slot for which generated transactions can stay valid
    pub max_age: u64,

    // -----------------
    // For TransactionProcessingCallback
    // -----------------
    pub feature_set: Arc<FeatureSet>,

    /// latest rent collector, knows the epoch
    rent_collector: RentCollector,

    /// FIFO queue of `recent_blockhash` items
    blockhash_queue: RwLock<BlockhashQueue>,

    // -----------------
    // Synchronization
    // -----------------
    /// Hash of this Bank's state. Only meaningful after freezing.
    /// NOTE: we need this for the `freeze_lock` synchronization
    hash: RwLock<Hash>,

    // -----------------
    // Cost
    // -----------------
    cost_tracker: RwLock<CostTracker>,

    // -----------------
    // Geyser
    // -----------------
    slot_status_notifier: Option<SlotStatusNotifierImpl>,
}

// -----------------
// TransactionProcessingCallback
// -----------------
impl TransactionProcessingCallback for Bank {
    // NOTE: main use is in solana/svm/src/transaction_processor.rs filter_executable_program_accounts
    // where it then uses the returned index to index into the [owners] array
    fn account_matches_owners(
        &self,
        account: &Pubkey,
        owners: &[Pubkey],
    ) -> Option<usize> {
        self.rc
            .accounts
            .accounts_db
            .account_matches_owners(account, owners)
            .ok()
    }

    fn get_account_shared_data(
        &self,
        pubkey: &Pubkey,
    ) -> Option<AccountSharedData> {
        self.rc.accounts.accounts_db.load(pubkey)
    }

    // NOTE: must hold idempotent for the same set of arguments
    /// Add a builtin program account
    fn add_builtin_account(&self, name: &str, program_id: &Pubkey) {
        let existing_genuine_program = self
            .get_account_with_fixed_root(program_id)
            .and_then(|account| {
                // it's very unlikely to be squatted at program_id as non-system account because of burden to
                // find victim's pubkey/hash. So, when account.owner is indeed native_loader's, it's
                // safe to assume it's a genuine program.
                if native_loader::check_id(account.owner()) {
                    Some(account)
                } else {
                    // malicious account is pre-occupying at program_id
                    self.burn_and_purge_account(program_id, account);
                    None
                }
            });

        // introducing builtin program
        if existing_genuine_program.is_some() {
            // The existing account is sufficient
            return;
        }

        assert!(
            !self.freeze_started(),
            "Can't change frozen bank by adding not-existing new builtin program ({name}, {program_id}). \
            Maybe, inconsistent program activation is detected on snapshot restore?"
        );

        // Add a bogus executable builtin account, which will be loaded and ignored.
        let account = native_loader::create_loadable_account_with_fields(
            name,
            self.inherit_specially_retained_account_fields(
                &existing_genuine_program,
            ),
        );
        self.store_account_and_update_capitalization(program_id, &account);
    }

    fn inspect_account(
        &self,
        _address: &Pubkey,
        _account_state: AccountState,
        _is_writable: bool,
    ) {
        // we don't need inspections
    }
}

#[derive(Default)]
pub struct TransactionExecutionRecordingOpts {
    pub enable_cpi_recording: bool,
    pub enable_log_recording: bool,
    pub enable_return_data_recording: bool,
}

impl TransactionExecutionRecordingOpts {
    pub fn recording_logs() -> Self {
        Self {
            enable_cpi_recording: false,
            enable_log_recording: true,
            enable_return_data_recording: false,
        }
    }

    pub fn recording_all() -> Self {
        Self {
            enable_cpi_recording: true,
            enable_log_recording: true,
            enable_return_data_recording: true,
        }
    }

    pub fn recording_all_if(condition: bool) -> Self {
        if condition {
            Self::recording_all()
        } else {
            Self::default()
        }
    }
}

impl Bank {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        genesis_config: &GenesisConfig,
        runtime_config: Arc<RuntimeConfig>,
        debug_keys: Option<Arc<HashSet<Pubkey>>>,
        additional_builtins: Option<&[BuiltinPrototype]>,
        debug_do_not_add_builtins: bool,
        accounts_paths: Vec<PathBuf>,
        accounts_update_notifier: Option<AccountsUpdateNotifier>,
        slot_status_notifier: Option<SlotStatusNotifierImpl>,
        millis_per_slot: u64,
        identity_id: Pubkey,
    ) -> Self {
        let accounts_path = accounts_paths
            .first()
            .expect("At least one accounts path is required")
            .to_path_buf();

        let accounts_db = AccountsDb::new_with_config(
            &genesis_config.cluster_type,
            accounts_update_notifier,
            accounts_paths,
        );

        let accounts = Accounts::new(Arc::new(accounts_db));
        let mut bank = Self::default_with_accounts(
            accounts,
            accounts_path,
            millis_per_slot,
        );
        bank.transaction_debug_keys = debug_keys;
        bank.runtime_config = runtime_config;
        bank.slot_status_notifier = slot_status_notifier;

        bank.process_genesis_config(genesis_config, identity_id);

        bank.finish_init(additional_builtins, debug_do_not_add_builtins);

        // NOTE: leaving out stake history sysvar setup

        // For more info about sysvars see ../../docs/sysvars.md

        // We don't really have epochs so we use the validator start time
        bank.update_clock(genesis_config.creation_time, None);
        bank.update_rent();
        bank.update_fees();
        bank.update_epoch_schedule();
        bank.update_last_restart_slot();

        // NOTE: the below sets those sysvars once and thus they stay the same for the lifetime of the bank
        // in our case we'd need to find a way to update at least the clock more regularly and set
        // it via bank.transaction_processor.sysvar_cache.write().unwrap().set_clock(), etc.
        bank.fill_missing_sysvar_cache_entries();

        // We don't have anything to verify at this point, so declare it done
        bank.set_startup_verification_complete();

        bank
    }

    pub(super) fn default_with_accounts(
        accounts: Accounts,
        accounts_path: PathBuf,
        millis_per_slot: u64,
    ) -> Self {
        // NOTE: this was not part of the original implementation

        // Transaction expiration needs to be a fixed amount of time
        // So we compute how many slots it takes for a transaction to expire
        // Depending on how fast each slot is computed
        let max_age = DEFAULT_MS_PER_SLOT * MAX_RECENT_BLOCKHASHES as u64
            / millis_per_slot;

        let mut bank = Self {
            rc: BankRc::new(accounts),
            slot: AtomicU64::default(),
            epoch: Epoch::default(),
            epoch_schedule: EpochSchedule::default(),
            is_delta: AtomicBool::default(),
            runtime_config: Arc::<RuntimeConfig>::default(),
            transaction_debug_keys: Option::<Arc<HashSet<Pubkey>>>::default(),
            transaction_log_collector_config: Arc::<
                RwLock<TransactionLogCollectorConfig>,
            >::default(),
            transaction_log_collector:
                Arc::<RwLock<TransactionLogCollector>>::default(),
            fee_structure: FeeStructure::default(),
            transaction_processor: Default::default(),
            fork_graph: Arc::<RwLock<SimpleForkGraph>>::default(),
            status_cache: Arc::new(RwLock::new(BankStatusCache::new(max_age))),
            millis_per_slot,
            max_age,
            identity_id: Pubkey::default(),
            accounts_path,

            // Counters
            transaction_count: AtomicU64::default(),
            non_vote_transaction_count_since_restart: AtomicU64::default(),
            transaction_error_count: AtomicU64::default(),
            transaction_entries_count: AtomicU64::default(),
            transactions_per_entry_max: AtomicU64::default(),
            accounts_data_size_delta_on_chain: AtomicI64::default(),
            accounts_data_size_delta_off_chain: AtomicI64::default(),
            signature_count: AtomicU64::default(),

            // Genesis related
            accounts_data_size_initial: 0,
            capitalization: AtomicU64::default(),
            fee_rate_governor: FeeRateGovernor::default(),
            max_tick_height: u64::default(),
            hashes_per_tick: Option::<u64>::default(),
            ticks_per_slot: u64::default(),
            ns_per_slot: u128::default(),
            genesis_creation_time: UnixTimestamp::default(),
            slots_per_year: f64::default(),

            // For TransactionProcessingCallback
            blockhash_queue: RwLock::new(BlockhashQueue::new(max_age as usize)),
            feature_set: Arc::<FeatureSet>::default(),
            rent_collector: RentCollector::default(),

            // Cost
            cost_tracker: RwLock::<CostTracker>::default(),

            // Synchronization
            hash: RwLock::<Hash>::default(),

            // Geyser
            slot_status_notifier: Option::<SlotStatusNotifierImpl>::default(),
        };

        bank.transaction_processor = {
            let tx_processor = TransactionBatchProcessor::new_uninitialized(
                bank.slot(),
                bank.epoch,
            );
            // NOTE: new anza impl requires this fork graph to be set
            tx_processor.program_cache.write().unwrap().set_fork_graph(
                Arc::<RwLock<SimpleForkGraph>>::downgrade(&bank.fork_graph),
            );
            RwLock::new(tx_processor)
        };

        bank
    }

    // -----------------
    // Init
    // -----------------
    fn finish_init(
        &mut self,
        additional_builtins: Option<&[BuiltinPrototype]>,
        debug_do_not_add_builtins: bool,
    ) {
        // NOTE: leaving out `rewards_pool_pubkeys` initialization

        self.apply_feature_activations();

        if !debug_do_not_add_builtins {
            for builtin in BUILTINS
                .iter()
                .chain(additional_builtins.unwrap_or(&[]).iter())
            {
                if builtin.feature_id.is_none() {
                    self.transaction_processor.read().unwrap().add_builtin(
                        self,
                        builtin.program_id,
                        builtin.name,
                        ProgramCacheEntry::new_builtin(
                            0,
                            builtin.name.len(),
                            builtin.entrypoint,
                        ),
                    );
                }
            }
            for precompile in get_precompiles() {
                if precompile.feature.is_none() {
                    self.add_precompile(&precompile.program_id);
                }
            }
        }

        {
            let txp = self.transaction_processor.read().unwrap();
            let mut loaded_programs_cache = txp.program_cache.write().unwrap();
            loaded_programs_cache.environments.program_runtime_v1 = Arc::new(
                create_program_runtime_environment_v1(
                    &self.feature_set,
                    &self.runtime_config.compute_budget.unwrap_or_default(),
                    false, /* deployment */
                    false, /* debugging_features */
                )
                .unwrap(),
            );
            loaded_programs_cache.environments.program_runtime_v2 =
                Arc::new(create_program_runtime_environment_v2(
                    &self.runtime_config.compute_budget.unwrap_or_default(),
                    false, /* debugging_features */
                ));
        }

        self.sync_loaded_programs_cache_to_slot();
    }

    fn sync_loaded_programs_cache_to_slot(&self) {
        let txp = self.transaction_processor.read().unwrap();
        let mut loaded_programs_cache = txp.program_cache.write().unwrap();
        loaded_programs_cache.latest_root_slot = self.slot();
        loaded_programs_cache.latest_root_epoch = self.epoch();
    }

    // -----------------
    // Genesis
    // -----------------
    fn process_genesis_config(
        &mut self,
        genesis_config: &GenesisConfig,
        identity_id: Pubkey,
    ) {
        // Bootstrap validator collects fees until `new_from_parent` is called.
        self.fee_rate_governor = genesis_config.fee_rate_governor.clone();

        // NOTE: these accounts can include feature activation accounts which need to be
        // present in order to properly activate a feature
        // If not then activating all features results in a panic when executing a transaction
        for (pubkey, account) in genesis_config.accounts.iter() {
            assert!(
                self.get_account(pubkey).is_none(),
                "{pubkey} repeated in genesis config"
            );
            self.store_account(*pubkey, account.clone().into());
            self.capitalization
                .fetch_add(account.lamports(), Ordering::Relaxed);
            self.accounts_data_size_initial += account.data().len() as u64;
        }

        debug!("set blockhash {:?}", genesis_config.hash());
        self.blockhash_queue.write().unwrap().genesis_hash(
            &genesis_config.hash(),
            self.fee_rate_governor.lamports_per_signature,
        );

        self.hashes_per_tick = genesis_config.hashes_per_tick();
        self.ticks_per_slot = genesis_config.ticks_per_slot();
        self.ns_per_slot = genesis_config.ns_per_slot();
        self.genesis_creation_time = genesis_config.creation_time;
        self.max_tick_height = (self.slot() + 1) * self.ticks_per_slot;
        self.slots_per_year = genesis_config.slots_per_year();

        self.epoch_schedule = genesis_config.epoch_schedule.clone();
        self.identity_id = identity_id;

        // Add additional builtin programs specified in the genesis config
        for (name, program_id) in &genesis_config.native_instruction_processors
        {
            self.add_builtin_account(name, program_id);
        }
    }

    pub fn get_identity(&self) -> Pubkey {
        self.identity_id
    }

    // -----------------
    // Slot, Epoch
    // -----------------
    pub fn slot(&self) -> Slot {
        self.slot.load(Ordering::Relaxed)
    }

    fn set_slot(&self, slot: Slot) {
        self.slot.store(slot, Ordering::Relaxed);
    }

    pub fn advance_slot(&self) -> Slot {
        // Determine next slot and set it
        let prev_slot = self.slot();
        let next_slot = prev_slot + 1;
        self.set_next_slot(next_slot);
        self.update_sysvars(self.genesis_creation_time, None);

        // Add a "root" to the status cache to trigger removing old items
        self.status_cache
            .write()
            .expect("RwLock of status cache poisoned")
            .add_root(prev_slot);

        // Determine next blockhash
        let current_hash = self.last_blockhash();
        let blockhash = {
            // In the Solana implementation there is a lot of logic going on to determine the next
            // blockhash, however we don't really produce any blocks, so any new hash will do.
            // Therefore we derive it from the previous hash and the current slot.
            let mut hasher = Hasher::default();
            hasher.hash(current_hash.as_ref());
            hasher.hash(&next_slot.to_le_bytes());
            hasher.result()
        };

        // Register the new blockhash with the blockhash queue
        {
            let mut blockhash_queue = self.blockhash_queue.write().unwrap();
            blockhash_queue.register_hash(
                &blockhash,
                self.fee_rate_governor.lamports_per_signature,
            );
        }

        // Notify Geyser Service
        if let Some(slot_status_notifier) = &self.slot_status_notifier {
            slot_status_notifier
                .notify_slot_rooted(next_slot, Some(next_slot - 1));
        }

        // Update loaded programs cache as otherwise we cannot deploy new programs
        self.sync_loaded_programs_cache_to_slot();

        self.update_slot_hashes_and_slot_history(prev_slot, current_hash);

        next_slot
    }

    pub fn epoch(&self) -> Epoch {
        self.epoch
    }

    pub fn epoch_schedule(&self) -> &EpochSchedule {
        &self.epoch_schedule
    }

    /// given a slot, return the epoch and offset into the epoch this slot falls
    /// e.g. with a fixed number for slots_per_epoch, the calculation is simply:
    ///
    ///  ( slot/slots_per_epoch, slot % slots_per_epoch )
    pub fn get_epoch_and_slot_index(&self, slot: Slot) -> (Epoch, SlotIndex) {
        self.epoch_schedule().get_epoch_and_slot_index(slot)
    }

    pub fn get_epoch_info(&self) -> EpochInfo {
        let absolute_slot = self.slot();
        let block_height = self.block_height();
        let (epoch, slot_index) = self.get_epoch_and_slot_index(absolute_slot);
        // One Epoch is roughly 2 days long and the Solana validator has a slot / 400ms
        // So, 2 days * 24 hours * 60 minutes * 60 seconds / 0.4 seconds = 432,000 slots
        let slots_in_epoch = self.get_slots_in_epoch(epoch);
        let transaction_count = Some(self.transaction_count());
        EpochInfo {
            epoch,
            slot_index,
            slots_in_epoch,
            absolute_slot,
            block_height,
            transaction_count,
        }
    }

    /// Return the number of slots per epoch for the given epoch
    pub fn get_slots_in_epoch(&self, epoch: Epoch) -> u64 {
        self.epoch_schedule().get_slots_in_epoch(epoch)
    }

    /// Return the block_height of this bank
    /// The number of blocks beneath the current block.
    /// The first block after the genesis block has height one.
    pub fn block_height(&self) -> u64 {
        self.slot()
    }

    // -----------------
    // Blockhash and Lamports
    // -----------------
    pub fn last_blockhash_and_lamports_per_signature(&self) -> (Hash, u64) {
        let blockhash_queue = self.blockhash_queue.read().unwrap();
        let last_hash = blockhash_queue.last_hash();
        let last_lamports_per_signature = blockhash_queue
            .get_lamports_per_signature(&last_hash)
            .unwrap(); // safe so long as the BlockhashQueue is consistent
        (last_hash, last_lamports_per_signature)
    }

    /// Return the last block hash registered.
    pub fn last_blockhash(&self) -> Hash {
        self.blockhash_queue.read().unwrap().last_hash()
    }

    pub fn get_blockhash_last_valid_block_height(
        &self,
        blockhash: &Hash,
    ) -> Option<Slot> {
        let blockhash_queue = self.blockhash_queue.read().unwrap();
        // This calculation will need to be updated to consider epoch boundaries if BlockhashQueue
        // length is made variable by epoch
        blockhash_queue.get_hash_age(blockhash).map(|age| {
            // Since we don't produce blocks ATM, we consider the current slot
            // to be our block height
            self.block_height() + MAX_PROCESSING_AGE as u64 - age
        })
    }

    // -----------------
    // Accounts
    // -----------------
    pub fn has_account(&self, pubkey: &Pubkey) -> bool {
        self.rc
            .accounts
            .accounts_db
            .accounts_cache
            .contains_key(pubkey)
    }

    pub fn get_account(&self, pubkey: &Pubkey) -> Option<AccountSharedData> {
        self.get_account_modified_slot(pubkey)
            .map(|(acc, _slot)| acc)
    }

    pub fn get_account_modified_slot(
        &self,
        pubkey: &Pubkey,
    ) -> Option<(AccountSharedData, Slot)> {
        self.load_slow(pubkey)
    }

    pub fn get_account_with_fixed_root(
        &self,
        pubkey: &Pubkey,
    ) -> Option<AccountSharedData> {
        self.get_account_modified_slot_with_fixed_root(pubkey)
            .map(|(acc, _slot)| acc)
    }

    pub fn get_account_modified_slot_with_fixed_root(
        &self,
        pubkey: &Pubkey,
    ) -> Option<(AccountSharedData, Slot)> {
        self.load_slow_with_fixed_root(pubkey)
    }

    fn load_slow(&self, pubkey: &Pubkey) -> Option<(AccountSharedData, Slot)> {
        self.rc.accounts.load_with_slot(pubkey)
    }

    fn load_slow_with_fixed_root(
        &self,
        pubkey: &Pubkey,
    ) -> Option<(AccountSharedData, Slot)> {
        self.rc.accounts.load_with_slot(pubkey)
    }

    /// fn store the single `account` with `pubkey`.
    /// Uses `store_accounts`, which works on a vector of accounts.
    pub fn store_account(&self, pubkey: Pubkey, account: AccountSharedData) {
        self.store_accounts(vec![(pubkey, account)])
    }

    /// Returns all the accounts this bank can load
    pub fn get_all_accounts(
        &self,
        sorted: bool,
    ) -> Vec<(Pubkey, AccountSharedData)> {
        self.rc.accounts.load_all(sorted)
    }

    pub fn store_accounts(&self, accounts: Vec<(Pubkey, AccountSharedData)>) {
        // NOTE: ideally we only have one bank and never freeze it
        // assert!(!self.freeze_started());
        //
        let mut m = Measure::start("stakes_cache.check_and_store");

        /* NOTE: for now disabled this part since we don't support staking
        let new_warmup_cooldown_rate_epoch = self.new_warmup_cooldown_rate_epoch();
        (0..accounts.len()).for_each(|i| {
            self.stakes_cache.check_and_store(
                accounts.pubkey(i),
                accounts.account(i),
                new_warmup_cooldown_rate_epoch,
            )
        });
        */
        let slot = self.slot();
        self.rc.accounts.store_accounts_cached(slot, accounts);
        m.stop();
        self.rc
            .accounts
            .accounts_db
            .stats
            .stakes_cache_check_and_store_us
            .fetch_add(m.as_us(), Ordering::Relaxed);
    }

    /// Technically this issues (or even burns!) new lamports,
    /// so be extra careful for its usage
    fn store_account_and_update_capitalization(
        &self,
        pubkey: &Pubkey,
        new_account: &AccountSharedData,
    ) {
        let old_account_data_size = if let Some(old_account) =
            self.get_account_with_fixed_root(pubkey)
        {
            match new_account.lamports().cmp(&old_account.lamports()) {
                std::cmp::Ordering::Greater => {
                    let increased =
                        new_account.lamports() - old_account.lamports();
                    trace!(
                            "store_account_and_update_capitalization: increased: {} {}",
                            pubkey,
                            increased
                        );
                    self.capitalization.fetch_add(increased, Ordering::Relaxed);
                }
                std::cmp::Ordering::Less => {
                    let decreased =
                        old_account.lamports() - new_account.lamports();
                    trace!(
                            "store_account_and_update_capitalization: decreased: {} {}",
                            pubkey,
                            decreased
                        );
                    self.capitalization.fetch_sub(decreased, Ordering::Relaxed);
                }
                std::cmp::Ordering::Equal => {}
            }
            old_account.data().len()
        } else {
            trace!(
                "store_account_and_update_capitalization: created: {} {}",
                pubkey,
                new_account.lamports()
            );
            self.capitalization
                .fetch_add(new_account.lamports(), Ordering::Relaxed);
            0
        };

        self.store_account(*pubkey, new_account.clone());
        self.calculate_and_update_accounts_data_size_delta_off_chain(
            old_account_data_size,
            new_account.data().len(),
        );
    }

    pub fn flush_accounts_cache(&self) -> AccountsDbResult<u64> {
        self.rc.accounts.accounts_db.flush_accounts_cache()
    }

    // -----------------
    // Transaction Accounts
    // -----------------
    pub fn unlock_accounts(&self, batch: &mut TransactionBatch) {
        if batch.needs_unlock() {
            batch.set_needs_unlock(false);
            self.rc.accounts.unlock_accounts(
                batch.sanitized_transactions().iter(),
                batch.lock_results(),
            )
        }
    }
    /// Get the max number of accounts that a transaction may lock in this block
    pub fn get_transaction_account_lock_limit(&self) -> usize {
        if let Some(transaction_account_lock_limit) =
            self.runtime_config.transaction_account_lock_limit
        {
            transaction_account_lock_limit
        } else {
            MAX_TX_ACCOUNT_LOCKS
        }
    }

    // -----------------
    // Balances
    // -----------------
    pub fn collect_balances(
        &self,
        batch: &TransactionBatch,
    ) -> TransactionBalances {
        let mut balances: TransactionBalances = vec![];
        for transaction in batch.sanitized_transactions() {
            let mut transaction_balances: Vec<u64> = vec![];
            for account_key in transaction.message().account_keys().iter() {
                transaction_balances.push(self.get_balance(account_key));
            }
            balances.push(transaction_balances);
        }
        balances
    }

    /// Each program would need to be able to introspect its own state
    /// this is hard-coded to the Budget language
    pub fn get_balance(&self, pubkey: &Pubkey) -> u64 {
        self.get_account(pubkey)
            .map(|x| Self::read_balance(&x))
            .unwrap_or(0)
    }

    pub fn read_balance(account: &AccountSharedData) -> u64 {
        account.lamports()
    }

    // -----------------
    // GetProgramAccounts
    // -----------------
    pub fn get_program_accounts(
        &self,
        program_id: &Pubkey,
        config: &ScanConfig,
    ) -> Vec<TransactionAccount> {
        self.rc.accounts.load_by_program(program_id, config)
    }

    pub fn get_filtered_program_accounts<F>(
        &self,
        program_id: &Pubkey,
        filter: F,
        config: &ScanConfig,
    ) -> Vec<TransactionAccount>
    where
        F: Fn(&AccountSharedData) -> bool + Send + Sync,
    {
        self.rc
            .accounts
            .load_by_program_with_filter(program_id, filter, config)
    }

    pub fn byte_limit_for_scans(&self) -> Option<usize> {
        // NOTE I cannot see where the retrieved value [AccountsIndexConfig::scan_results_limit_bytes]
        // solana/accounts-db/src/accounts_index.rs :217
        // is configured, so we assume this is fine for now
        None
    }

    // -----------------
    // SysVars
    // -----------------
    pub fn clock(&self) -> sysvar::clock::Clock {
        from_account(
            &self.get_account(&sysvar::clock::id()).unwrap_or_default(),
        )
        .unwrap_or_default()
    }

    fn update_clock(
        &self,
        epoch_start_timestamp: UnixTimestamp,
        timestamp: Option<UnixTimestamp>,
    ) {
        // NOTE: the Solana validator determines time with a much more complex logic
        // - slot == 0: genesis creation time + number of slots * ns_per_slot to seconds
        // - slot > 0 : epoch start time + number of slots to get a timestamp estimate with max
        //              allowable drift
        // Different timestamp votes are then considered, taking stake into account and the median
        // is used as the final value.
        // Possibly for that reason the solana UnixTimestamp is an i64 in order to make those
        // calculations easier.
        // This makes sense since otherwise the hosting platform could manipulate the time assumed
        // by the validator.
        let unix_timestamp = timestamp.unwrap_or_else(|| {
            i64::try_from(get_epoch_secs()).expect("get_epoch_secs overflow")
        });

        // I checked this against crate::bank_helpers::get_sys_time_in_secs();
        // and confirmed that the timestamps match

        let slot = self.slot();
        let clock = sysvar::clock::Clock {
            slot,
            epoch_start_timestamp,
            epoch: self.epoch_schedule().get_epoch(slot),
            leader_schedule_epoch: self
                .epoch_schedule()
                .get_leader_schedule_epoch(slot),
            unix_timestamp,
        };
        self.update_sysvar_account(&sysvar::clock::id(), |account| {
            create_account(
                &clock,
                inherit_specially_retained_account_fields(account),
            )
        });
        self.set_clock_in_sysvar_cache(clock);
    }

    fn update_rent(&self) {
        self.update_sysvar_account(&sysvar::rent::id(), |account| {
            create_account(
                &self.rent_collector.rent,
                inherit_specially_retained_account_fields(account),
            )
        });
    }

    #[allow(deprecated)]
    fn update_fees(&self) {
        if !self
            .feature_set
            .is_active(&feature_set::disable_fees_sysvar::id())
        {
            self.update_sysvar_account(&sysvar::fees::id(), |account| {
                create_account(
                    &sysvar::fees::Fees::new(
                        &self.fee_rate_governor.create_fee_calculator(),
                    ),
                    inherit_specially_retained_account_fields(account),
                )
            });
        }
    }

    fn update_epoch_schedule(&self) {
        self.update_sysvar_account(&sysvar::epoch_schedule::id(), |account| {
            create_account(
                self.epoch_schedule(),
                inherit_specially_retained_account_fields(account),
            )
        });
    }

    fn update_slot_history(&self, slot: Slot) {
        self.update_sysvar_account(&sysvar::slot_history::id(), |account| {
            let mut slot_history = account
                .as_ref()
                .map(|account| from_account::<SlotHistory, _>(account).unwrap())
                .unwrap_or_default();
            slot_history.add(slot);
            create_account(
                &slot_history,
                inherit_specially_retained_account_fields(account),
            )
        });
    }
    fn update_slot_hashes(&self, prev_slot: Slot, prev_hash: Hash) {
        self.update_sysvar_account(&sysvar::slot_hashes::id(), |account| {
            let mut slot_hashes = account
                .as_ref()
                .map(|account| from_account::<SlotHashes, _>(account).unwrap())
                .unwrap_or_default();
            slot_hashes.add(prev_slot, prev_hash);
            create_account(
                &slot_hashes,
                inherit_specially_retained_account_fields(account),
            )
        });
    }

    pub fn update_last_restart_slot(&self) {
        let feature_flag = self
            .feature_set
            .is_active(&feature_set::last_restart_slot_sysvar::id());

        if feature_flag {
            // First, see what the currently stored last restart slot is. This
            // account may not exist yet if the feature was just activated.
            let current_last_restart_slot = self
                .get_account(&sysvar::last_restart_slot::id())
                .and_then(|account| {
                    let lrs: Option<LastRestartSlot> = from_account(&account);
                    lrs
                })
                .map(|account| account.last_restart_slot);

            let last_restart_slot = 0;
            // NOTE: removed querying hard forks here

            // Only need to write if the last restart has changed
            if current_last_restart_slot != Some(last_restart_slot) {
                self.update_sysvar_account(
                    &sysvar::last_restart_slot::id(),
                    |account| {
                        create_account(
                            &LastRestartSlot { last_restart_slot },
                            inherit_specially_retained_account_fields(account),
                        )
                    },
                );
            }
        }
    }

    fn update_sysvar_account<F>(&self, pubkey: &Pubkey, updater: F)
    where
        F: Fn(&Option<AccountSharedData>) -> AccountSharedData,
    {
        let old_account = self.get_account_with_fixed_root(pubkey);
        let mut new_account = updater(&old_account);

        // When new sysvar comes into existence (with RENT_UNADJUSTED_INITIAL_BALANCE lamports),
        // this code ensures that the sysvar's balance is adjusted to be rent-exempt.
        //
        // More generally, this code always re-calculates for possible sysvar data size change,
        // although there is no such sysvars currently.
        self.adjust_sysvar_balance_for_rent(&mut new_account);
        self.store_account_and_update_capitalization(pubkey, &new_account);
    }

    fn adjust_sysvar_balance_for_rent(&self, account: &mut AccountSharedData) {
        account.set_lamports(
            self.get_minimum_balance_for_rent_exemption(account.data().len())
                .max(account.lamports()),
        );
    }

    pub fn get_minimum_balance_for_rent_exemption(
        &self,
        data_len: usize,
    ) -> u64 {
        self.rent_collector.rent.minimum_balance(data_len).max(1)
    }

    pub fn is_blockhash_valid_for_age(&self, hash: &Hash) -> bool {
        let blockhash_queue = self.blockhash_queue.read().unwrap();
        blockhash_queue.is_hash_valid_for_age(hash, self.max_age as usize)
    }

    // -----------------
    // Features
    // -----------------
    // In Solana this is called from snapshot restore AND for each epoch boundary
    // The entire code path herein must be idempotent
    // In our case only during finish_init when the bank is created
    fn apply_feature_activations(&mut self) {
        let feature_set = self.compute_active_feature_set();
        // NOTE: at this point we have only inactive features
        self.feature_set = Arc::new(feature_set);
    }

    /// Compute the active feature set based on the current bank state,
    /// and return it together with the set of newly activated features (we don't).
    fn compute_active_feature_set(&self) -> FeatureSet {
        // NOTE: took out the `pending` features since we don't support new feature activations
        // which in Solana only are used when we create a bank from a parent bank
        let mut active = self.feature_set.active.clone();
        let mut inactive = HashSet::new();
        let slot = self.slot();

        for feature_id in &self.feature_set.inactive {
            let mut activated = None;
            if let Some(account) = self.get_account_with_fixed_root(feature_id)
            {
                if let Some(feature) = feature::from_account(&account) {
                    match feature.activated_at {
                        Some(activation_slot) if slot >= activation_slot => {
                            // Feature has been activated already
                            activated = Some(activation_slot);
                        }
                        _ => {}
                    }
                }
            }
            if let Some(slot) = activated {
                active.insert(*feature_id, slot);
            } else {
                inactive.insert(*feature_id);
            }
        }

        FeatureSet { active, inactive }
    }

    // Looks like this is only used in tests since add_precompiled_account_with_owner is as well
    // However `finish_init` is calling this method, so we keep it here
    pub fn add_precompile(&mut self, program_id: &Pubkey) {
        debug!("Adding precompiled program {}", program_id);
        self.add_precompiled_account(program_id);
    }

    /// Add a precompiled program account
    pub fn add_precompiled_account(&self, program_id: &Pubkey) {
        self.add_precompiled_account_with_owner(program_id, native_loader::id())
    }

    // Used by tests to simulate clusters with precompiles that aren't owned by the native loader
    fn add_precompiled_account_with_owner(
        &self,
        program_id: &Pubkey,
        owner: Pubkey,
    ) {
        if let Some(account) = self.get_account_with_fixed_root(program_id) {
            if account.executable() {
                return;
            }
            // malicious account is pre-occupying at program_id
            self.burn_and_purge_account(program_id, account);
        };

        assert!(
            !self.freeze_started(),
            "Can't change frozen bank by adding not-existing new precompiled program ({program_id}). \
                Maybe, inconsistent program activation is detected on snapshot restore?"
        );

        // Add a bogus executable account, which will be loaded and ignored.
        let (lamports, rent_epoch) =
            inherit_specially_retained_account_fields(&None);

        let account = AccountSharedData::from(Account {
            lamports,
            owner,
            data: vec![],
            executable: true,
            rent_epoch,
        });
        self.store_account_and_update_capitalization(program_id, &account);
    }

    fn burn_and_purge_account(
        &self,
        program_id: &Pubkey,
        mut account: AccountSharedData,
    ) {
        let old_data_size = account.data().len();
        self.capitalization
            .fetch_sub(account.lamports(), Ordering::Relaxed);
        // Both resetting account balance to 0 and zeroing the account data
        // is needed to really purge from AccountsDb and flush the Stakes cache
        account.set_lamports(0);
        account.data_as_mut_slice().fill(0);
        self.store_account(*program_id, account);
        self.calculate_and_update_accounts_data_size_delta_off_chain(
            old_data_size,
            0,
        );
    }

    // -----------------
    // Transaction Preparation
    // -----------------
    /// Prepare a locked transaction batch from a list of sanitized transactions.
    pub fn prepare_sanitized_batch<'a, 'b>(
        &'a self,
        txs: &'b [SanitizedTransaction],
    ) -> TransactionBatch<'a, 'b> {
        let tx_account_lock_limit = self.get_transaction_account_lock_limit();
        let lock_results = self
            .rc
            .accounts
            .lock_accounts(txs.iter(), tx_account_lock_limit);
        TransactionBatch::new(lock_results, self, Cow::Borrowed(txs))
    }

    /// Prepare a locked transaction batch from a list of sanitized transactions, and their cost
    /// limited packing status
    pub fn prepare_sanitized_batch_with_results<'a, 'b>(
        &'a self,
        transactions: &'b [SanitizedTransaction],
        transaction_results: impl Iterator<Item = Result<()>>,
    ) -> TransactionBatch<'a, 'b> {
        // this lock_results could be: Ok, AccountInUse, WouldExceedBlockMaxLimit or WouldExceedAccountMaxLimit
        let tx_account_lock_limit = self.get_transaction_account_lock_limit();
        let lock_results = self.rc.accounts.lock_accounts_with_results(
            transactions.iter(),
            transaction_results,
            tx_account_lock_limit,
        );
        TransactionBatch::new(lock_results, self, Cow::Borrowed(transactions))
    }

    // -----------------
    // Transaction Checking
    // -----------------
    pub fn check_transactions(
        &self,
        sanitized_txs: &[impl core::borrow::Borrow<SanitizedTransaction>],
        lock_results: &[Result<()>],
        error_counters: &mut TransactionErrorMetrics,
    ) -> Vec<TransactionCheckResult> {
        let age_results =
            self.check_age(sanitized_txs, lock_results, error_counters);
        self.check_status_cache(sanitized_txs, age_results, error_counters)
    }

    fn check_age(
        &self,
        sanitized_txs: &[impl core::borrow::Borrow<SanitizedTransaction>],
        lock_results: &[solana_sdk::transaction::Result<()>],
        error_counters: &mut TransactionErrorMetrics,
    ) -> Vec<TransactionCheckResult> {
        let hash_queue = self.blockhash_queue.read().unwrap();
        let last_blockhash = hash_queue.last_hash();
        let next_durable_nonce = DurableNonce::from_blockhash(&last_blockhash);
        // safe so long as the BlockhashQueue is consistent
        let next_lamports_per_signature = hash_queue
            .get_lamports_per_signature(&last_blockhash)
            .unwrap();

        sanitized_txs
            .iter()
            .zip(lock_results)
            .map(|(tx, lock_res)| match lock_res {
                Ok(()) => self.check_transaction_age(
                    tx.borrow(),
                    &next_durable_nonce,
                    &hash_queue,
                    next_lamports_per_signature,
                    error_counters,
                ),
                Err(e) => Err(e.clone()),
            })
            .collect()
    }

    fn check_transaction_age(
        &self,
        tx: &SanitizedTransaction,
        next_durable_nonce: &DurableNonce,
        hash_queue: &BlockhashQueue,
        next_lamports_per_signature: u64,
        error_counters: &mut TransactionErrorMetrics,
    ) -> TransactionCheckResult {
        let max_age = self.max_age as usize;
        let recent_blockhash = tx.message().recent_blockhash();
        if let Some(hash_info) =
            hash_queue.get_hash_info_if_valid(recent_blockhash, max_age)
        {
            Ok(CheckedTransactionDetails {
                nonce: None,
                lamports_per_signature: hash_info.lamports_per_signature(),
            })
        } else if let Some((nonce, previous_lamports_per_signature)) = self
            .check_load_and_advance_message_nonce_account(
                tx.message(),
                next_durable_nonce,
                next_lamports_per_signature,
            )
        {
            Ok(CheckedTransactionDetails {
                nonce: Some(nonce),
                lamports_per_signature: previous_lamports_per_signature,
            })
        } else {
            error_counters.blockhash_not_found += 1;
            Err(TransactionError::BlockhashNotFound)
        }
    }
    pub(super) fn check_load_and_advance_message_nonce_account(
        &self,
        message: &SanitizedMessage,
        next_durable_nonce: &DurableNonce,
        next_lamports_per_signature: u64,
    ) -> Option<(NonceInfo, u64)> {
        let nonce_is_advanceable =
            message.recent_blockhash() != next_durable_nonce.as_hash();
        if !nonce_is_advanceable {
            return None;
        }

        let (nonce_address, mut nonce_account, nonce_data) =
            self.load_message_nonce_account(message)?;

        let previous_lamports_per_signature =
            nonce_data.get_lamports_per_signature();
        let next_nonce_state = nonce::state::State::new_initialized(
            &nonce_data.authority,
            *next_durable_nonce,
            next_lamports_per_signature,
        );
        nonce_account
            .set_state(&nonce::state::Versions::new(next_nonce_state))
            .ok()?;

        Some((
            NonceInfo::new(nonce_address, nonce_account),
            previous_lamports_per_signature,
        ))
    }

    pub(super) fn load_message_nonce_account(
        &self,
        message: &SanitizedMessage,
    ) -> Option<(Pubkey, AccountSharedData, nonce::state::Data)> {
        let nonce_address = message.get_durable_nonce()?;
        let nonce_account = self.get_account_with_fixed_root(nonce_address)?;
        let nonce_data = nonce_account::verify_nonce_account(
            &nonce_account,
            message.recent_blockhash(),
        )?;

        let nonce_is_authorized = message
            .get_ix_signers(NONCED_TX_MARKER_IX_INDEX as usize)
            .any(|signer| signer == &nonce_data.authority);
        if !nonce_is_authorized {
            return None;
        }

        Some((*nonce_address, nonce_account, nonce_data))
    }

    fn is_transaction_already_processed(
        &self,
        sanitized_tx: &SanitizedTransaction,
        status_cache: &BankStatusCache,
    ) -> bool {
        let signature = sanitized_tx.signature();
        status_cache
            .get_recent_transaction_status(signature, Some(self.max_age))
            .is_some()
    }

    fn check_status_cache(
        &self,
        sanitized_txs: &[impl core::borrow::Borrow<SanitizedTransaction>],
        lock_results: Vec<TransactionCheckResult>,
        error_counters: &mut TransactionErrorMetrics,
    ) -> Vec<TransactionCheckResult> {
        let rcache = self.status_cache.read().unwrap();
        sanitized_txs
            .iter()
            .zip(lock_results)
            .map(|(sanitized_tx, lock_result)| {
                let sanitized_tx = sanitized_tx.borrow();
                if lock_result.is_ok()
                    && self
                        .is_transaction_already_processed(sanitized_tx, &rcache)
                {
                    error_counters.already_processed += 1;
                    return Err(TransactionError::AlreadyProcessed);
                }

                lock_result
            })
            .collect()
    }

    // -----------------
    // Transaction Execution
    // -----------------
    pub fn load_and_execute_transactions(
        &self,
        batch: &TransactionBatch,
        timings: &mut ExecuteTimings,
        error_counters: &mut TransactionErrorMetrics,
        processing_config: TransactionProcessingConfig,
    ) -> LoadAndExecuteTransactionsOutput {
        let sanitized_txs = batch.sanitized_transactions();

        let (check_results, check_us) = measure_us!(self.check_transactions(
            sanitized_txs,
            batch.lock_results(),
            error_counters,
        ));
        timings.saturating_add_in_place(ExecuteTimingType::CheckUs, check_us);

        let (blockhash, lamports_per_signature) =
            self.last_blockhash_and_lamports_per_signature();
        let processing_environment = TransactionProcessingEnvironment {
            blockhash,
            epoch_total_stake: None,
            epoch_vote_accounts: None,
            feature_set: Arc::clone(&self.feature_set),
            fee_structure: Some(&self.fee_structure),
            lamports_per_signature,
            rent_collector: None,
        };

        let sanitized_output = self
            .transaction_processor
            .read()
            .unwrap()
            .load_and_execute_sanitized_transactions(
                self,
                sanitized_txs,
                check_results,
                &processing_environment,
                &processing_config,
            );

        // Accumulate the errors returned by the batch processor.
        error_counters.accumulate(&sanitized_output.error_metrics);

        // Accumulate the transaction batch execution timings.
        timings.accumulate(&sanitized_output.execute_timings);

        let mut processed_counts = ProcessedTransactionCounts::default();
        let err_count = &mut error_counters.total;

        for (processing_result, tx) in sanitized_output
            .processing_results
            .iter()
            .zip(sanitized_txs)
        {
            if let Some(debug_keys) = &self.transaction_debug_keys {
                for key in tx.message().account_keys().iter() {
                    if debug_keys.contains(key) {
                        let result = processing_result.flattened_result();
                        info!(
                            "slot: {} result: {:?} tx: {:?}",
                            self.slot.load(Ordering::Relaxed),
                            result,
                            tx
                        );
                        break;
                    }
                }
            }

            if processing_result.was_processed() {
                // Signature count must be accumulated only if the transaction
                // is processed, otherwise a mismatched count between banking
                // and replay could occur
                processed_counts.signature_count +=
                    u64::from(tx.message().header().num_required_signatures);
                processed_counts.processed_transactions_count += 1;

                if !tx.is_simple_vote_transaction() {
                    processed_counts.processed_non_vote_transactions_count += 1;
                }
            }

            match processing_result.flattened_result() {
                Ok(()) => {
                    processed_counts.processed_with_successful_result_count +=
                        1;
                }
                Err(err) => {
                    if *err_count == 0 {
                        debug!("tx error: {:?} {:?}", err, tx);
                    }
                    *err_count += 1;
                }
            }
        }

        LoadAndExecuteTransactionsOutput {
            processing_results: sanitized_output.processing_results,
            processed_counts,
        }
    }

    /// Process a batch of transactions.
    #[must_use]
    pub fn load_execute_and_commit_transactions(
        &self,
        batch: &TransactionBatch,
        collect_balances: bool,
        recording_config: ExecutionRecordingConfig,
        timings: &mut ExecuteTimings,
        log_messages_bytes_limit: Option<usize>,
    ) -> (Vec<TransactionCommitResult>, TransactionBalancesSet) {
        let pre_balances = if collect_balances {
            self.collect_balances(batch)
        } else {
            vec![]
        };

        let LoadAndExecuteTransactionsOutput {
            processing_results,
            processed_counts,
        } = self.load_and_execute_transactions(
            batch,
            timings,
            &mut TransactionErrorMetrics::default(),
            TransactionProcessingConfig {
                account_overrides: None,
                check_program_modification_slot: false,
                compute_budget: None,
                log_messages_bytes_limit,
                limit_to_load_programs: false,
                recording_config,
                transaction_account_lock_limit: None,
            },
        );

        let commit_results = self.commit_transactions(
            batch.sanitized_transactions(),
            processing_results,
            &processed_counts,
            timings,
        );
        let post_balances = if collect_balances {
            self.collect_balances(batch)
        } else {
            vec![]
        };
        (
            commit_results,
            TransactionBalancesSet::new(pre_balances, post_balances),
        )
    }

    /// `committed_transactions_count` is the number of transactions out of `sanitized_txs`
    /// that was executed. Of those, `committed_transactions_count`,
    /// `committed_with_failure_result_count` is the number of executed transactions that returned
    /// a failure result.
    #[allow(clippy::too_many_arguments)]
    pub fn commit_transactions(
        &self,
        sanitized_txs: &[SanitizedTransaction],
        processing_results: Vec<TransactionProcessingResult>,
        processed_counts: &ProcessedTransactionCounts,
        timings: &mut ExecuteTimings,
    ) -> Vec<TransactionCommitResult> {
        assert!(
            !self.freeze_started(),
            "commit_transactions() working on a bank that is already frozen or is undergoing freezing!"
        );

        let ProcessedTransactionCounts {
            processed_transactions_count,
            processed_non_vote_transactions_count,
            processed_with_successful_result_count,
            signature_count,
        } = *processed_counts;

        self.increment_transaction_count(processed_transactions_count);
        self.increment_non_vote_transaction_count_since_restart(
            processed_non_vote_transactions_count,
        );
        self.increment_signature_count(signature_count);

        let processed_with_failure_result_count = processed_transactions_count
            .saturating_sub(processed_with_successful_result_count);
        self.transaction_error_count
            .fetch_add(processed_with_failure_result_count, Ordering::Relaxed);

        if processed_transactions_count > 0 {
            self.is_delta.store(true, Ordering::Relaxed);
            self.transaction_entries_count
                .fetch_add(1, Ordering::Relaxed);
            self.transactions_per_entry_max
                .fetch_max(processed_transactions_count, Ordering::Relaxed);
        }

        let ((), store_accounts_us) = measure_us!({
            self.rc.accounts.store_cached(
                self.slot(),
                sanitized_txs,
                &processing_results,
            );
        });
        let ((), update_executors_us) = measure_us!({
            let txp = self.transaction_processor.read().unwrap();
            let mut cache = txp.program_cache.write().unwrap();
            for processing_result in &processing_results {
                if let Some(ProcessedTransaction::Executed(executed_tx)) =
                    processing_result.processed_transaction()
                {
                    let programs_modified_by_tx =
                        &executed_tx.programs_modified_by_tx;
                    if executed_tx.was_successful()
                        && !programs_modified_by_tx.is_empty()
                    {
                        cache.merge(programs_modified_by_tx);
                    }
                }
            }
        });

        let accounts_data_len_delta = processing_results
            .iter()
            .filter_map(|processing_result| {
                processing_result.processed_transaction()
            })
            .filter_map(|processed_tx| processed_tx.execution_details())
            .filter_map(|details| {
                details
                    .status
                    .is_ok()
                    .then_some(details.accounts_data_len_delta)
            })
            .sum();
        self.update_accounts_data_size_delta_on_chain(accounts_data_len_delta);

        let ((), update_transaction_statuses_us) = measure_us!(self
            .update_transaction_statuses(sanitized_txs, &processing_results));

        self.filter_program_errors_and_collect_fee(&processing_results);

        timings.saturating_add_in_place(
            ExecuteTimingType::StoreUs,
            store_accounts_us,
        );
        timings.saturating_add_in_place(
            ExecuteTimingType::UpdateExecutorsUs,
            update_executors_us,
        );
        timings.saturating_add_in_place(
            ExecuteTimingType::UpdateTransactionStatuses,
            update_transaction_statuses_us,
        );

        Self::create_commit_results(processing_results)
    }

    fn create_commit_results(
        processing_results: Vec<TransactionProcessingResult>,
    ) -> Vec<TransactionCommitResult> {
        processing_results
            .into_iter()
            .map(|processing_result| match processing_result? {
                ProcessedTransaction::Executed(executed_tx) => {
                    let execution_details = executed_tx.execution_details;
                    let LoadedTransaction {
                        rent_debits,
                        accounts: loaded_accounts,
                        loaded_accounts_data_size,
                        fee_details,
                        ..
                    } = executed_tx.loaded_transaction;

                    // Rent is only collected for successfully executed transactions
                    let rent_debits = if execution_details.was_successful() {
                        rent_debits
                    } else {
                        RentDebits::default()
                    };

                    Ok(CommittedTransaction {
                        status: execution_details.status,
                        log_messages: execution_details.log_messages,
                        inner_instructions: execution_details
                            .inner_instructions,
                        return_data: execution_details.return_data,
                        executed_units: execution_details.executed_units,
                        fee_details,
                        rent_debits,
                        loaded_account_stats: TransactionLoadedAccountsStats {
                            loaded_accounts_count: loaded_accounts.len(),
                            loaded_accounts_data_size,
                        },
                    })
                }
                ProcessedTransaction::FeesOnly(fees_only_tx) => {
                    Ok(CommittedTransaction {
                        status: Err(fees_only_tx.load_error),
                        log_messages: None,
                        inner_instructions: None,
                        return_data: None,
                        executed_units: 0,
                        rent_debits: RentDebits::default(),
                        fee_details: fees_only_tx.fee_details,
                        loaded_account_stats: TransactionLoadedAccountsStats {
                            loaded_accounts_count: fees_only_tx
                                .rollback_accounts
                                .count(),
                            loaded_accounts_data_size: fees_only_tx
                                .rollback_accounts
                                .data_size()
                                as u32,
                        },
                    })
                }
            })
            .collect()
    }

    fn update_transaction_statuses(
        &self,
        sanitized_txs: &[SanitizedTransaction],
        processing_results: &[TransactionProcessingResult],
    ) {
        let mut status_cache = self.status_cache.write().unwrap();
        assert_eq!(sanitized_txs.len(), processing_results.len());
        for (tx, processing_result) in
            sanitized_txs.iter().zip(processing_results)
        {
            if let Ok(processed_tx) = &processing_result {
                // Add the message hash to the status cache to ensure that this message
                // won't be processed again with a different signature.
                status_cache.insert(
                    tx.message().recent_blockhash(),
                    tx.message_hash(),
                    self.slot(),
                    processed_tx.status(),
                );
                // Add the transaction signature to the status cache so that transaction status
                // can be queried by transaction signature over RPC. In the future, this should
                // only be added for API nodes because voting validators don't need to do this.
                status_cache.insert(
                    tx.message().recent_blockhash(),
                    tx.signature(),
                    self.slot(),
                    processed_tx.status(),
                );
                // Additionally update the transaction status cache by slot to allow quickly
                // finding transactions by going backward in time until a specific slot
                status_cache.insert_transaction_status(
                    self.slot(),
                    tx.signature(),
                    processed_tx.status(),
                );
            }
        }
    }

    fn filter_program_errors_and_collect_fee(
        &self,
        processing_results: &[TransactionProcessingResult],
    ) {
        let mut fees = 0;

        processing_results.iter().for_each(|processing_result| {
            if let Ok(processed_tx) = processing_result {
                fees += processed_tx.fee_details().total_fee();
            }
        });
    }

    // -----------------
    // Transaction Verification
    // -----------------
    pub fn verify_transaction(
        &self,
        tx: VersionedTransaction,
        verification_mode: TransactionVerificationMode,
    ) -> Result<SanitizedTransaction> {
        let sanitized_tx = {
            let size = bincode::serialized_size(&tx)
                .map_err(|_| TransactionError::SanitizeFailure)?;
            if size > PACKET_DATA_SIZE as u64 {
                return Err(TransactionError::SanitizeFailure);
            }
            let message_hash = if verification_mode
                == TransactionVerificationMode::FullVerification
            {
                tx.verify_and_hash_message()?
            } else {
                tx.message.hash()
            };

            SanitizedTransaction::try_create(
                tx,
                message_hash,
                None,
                self,
                &HashSet::new(),
            )
        }?;

        if verification_mode
            == TransactionVerificationMode::HashAndVerifyPrecompiles
            || verification_mode
                == TransactionVerificationMode::FullVerification
        {
            sanitized_tx.verify_precompiles(&self.feature_set)?;
        }

        Ok(sanitized_tx)
    }

    pub fn fully_verify_transaction(
        &self,
        tx: VersionedTransaction,
    ) -> Result<SanitizedTransaction> {
        self.verify_transaction(
            tx,
            TransactionVerificationMode::FullVerification,
        )
    }

    pub fn get_lamports_per_signature(&self) -> u64 {
        self.fee_rate_governor.lamports_per_signature
    }

    pub fn get_fee_for_message(
        &self,
        message: &SanitizedMessage,
    ) -> Option<u64> {
        let lamports_per_signature = {
            let blockhash_queue = self.blockhash_queue.read().unwrap();
            blockhash_queue
                .get_lamports_per_signature(message.recent_blockhash())
        }
        .or_else(|| {
            self.load_message_nonce_account(message).map(
                |(_nonce_address, _nonce_account, nonce_data)| {
                    nonce_data.get_lamports_per_signature()
                },
            )
        })?;
        Some(self.get_fee_for_message_with_lamports_per_signature(
            message,
            lamports_per_signature,
        ))
    }

    pub fn get_fee_for_message_with_lamports_per_signature(
        &self,
        message: &impl SVMMessage,
        lamports_per_signature: u64,
    ) -> u64 {
        let fee_budget_limits = FeeBudgetLimits::from(
            process_compute_budget_instructions(
                message.program_instructions_iter(),
                &self.feature_set,
            )
            .unwrap_or_default(),
        );
        solana_fee::calculate_fee(
            message,
            lamports_per_signature == 0,
            self.fee_structure.lamports_per_signature,
            fee_budget_limits.prioritization_fee,
            false,
        )
    }

    // -----------------
    // Simulate Transaction
    // -----------------
    /// Run transactions against a bank without committing the results; does not check if the bank
    /// is frozen like Solana does to enable use in single-bank scenarios
    pub fn simulate_transaction_unchecked(
        &self,
        transaction: &SanitizedTransaction,
        enable_cpi_recording: bool,
    ) -> TransactionSimulationResult {
        let account_keys = transaction.message().account_keys();
        let number_of_accounts = account_keys.len();
        let account_overrides =
            self.get_account_overrides_for_simulation(&account_keys);
        let batch = self.prepare_unlocked_batch_from_single_tx(transaction);
        let mut timings = ExecuteTimings::default();

        let LoadAndExecuteTransactionsOutput {
            mut processing_results,
            ..
        } = self.load_and_execute_transactions(
            &batch,
            // After simulation, transactions will need to be forwarded to the leader
            // for processing. During forwarding, the transaction could expire if the
            // delay is not accounted for.
            &mut timings,
            &mut TransactionErrorMetrics::default(),
            TransactionProcessingConfig {
                account_overrides: Some(&account_overrides),
                check_program_modification_slot: false,
                compute_budget: None,
                log_messages_bytes_limit: None,
                limit_to_load_programs: true,
                recording_config: ExecutionRecordingConfig {
                    enable_cpi_recording,
                    enable_log_recording: true,
                    enable_return_data_recording: true,
                },
                transaction_account_lock_limit: Some(
                    self.get_transaction_account_lock_limit(),
                ),
            },
        );

        let units_consumed = timings.details.per_program_timings.iter().fold(
            0,
            |acc: u64, (_, program_timing)| {
                acc.saturating_add(program_timing.accumulated_units)
                    .saturating_add(program_timing.total_errored_units)
            },
        );

        debug!("simulate_transaction: {:?}", timings);

        let processing_result = processing_results
            .pop()
            .unwrap_or(Err(TransactionError::InvalidProgramForExecution));
        let (
            post_simulation_accounts,
            result,
            logs,
            return_data,
            inner_instructions,
        ) = match processing_result {
            Ok(processed_tx) => match processed_tx {
                ProcessedTransaction::Executed(executed_tx) => {
                    let details = executed_tx.execution_details;
                    let post_simulation_accounts = executed_tx
                        .loaded_transaction
                        .accounts
                        .into_iter()
                        .take(number_of_accounts)
                        .collect::<Vec<_>>();
                    (
                        post_simulation_accounts,
                        details.status,
                        details.log_messages,
                        details.return_data,
                        details.inner_instructions,
                    )
                }
                ProcessedTransaction::FeesOnly(fees_only_tx) => {
                    (vec![], Err(fees_only_tx.load_error), None, None, None)
                }
            },
            Err(error) => (vec![], Err(error), None, None, None),
        };
        let logs = logs.unwrap_or_default();

        TransactionSimulationResult {
            result,
            logs,
            post_simulation_accounts,
            units_consumed,
            return_data,
            inner_instructions,
        }
    }

    fn get_account_overrides_for_simulation(
        &self,
        account_keys: &AccountKeys,
    ) -> AccountOverrides {
        let mut account_overrides = AccountOverrides::default();
        let slot_history_id = sysvar::slot_history::id();
        // For now this won't run properly since we don't support slot_history sysvar
        if account_keys.iter().any(|pubkey| *pubkey == slot_history_id) {
            let current_account =
                self.get_account_with_fixed_root(&slot_history_id);
            let slot_history = current_account
                .as_ref()
                .map(|account| from_account::<SlotHistory, _>(account).unwrap())
                .unwrap_or_default();
            if slot_history.check(self.slot()) == Check::Found {
                if let Some((account, _)) =
                    self.load_slow_with_fixed_root(&slot_history_id)
                {
                    account_overrides.set_slot_history(Some(account));
                }
            }
        }
        account_overrides
    }

    /// Prepare a transaction batch from a single transaction without locking accounts
    fn prepare_unlocked_batch_from_single_tx<'a>(
        &'a self,
        transaction: &'a SanitizedTransaction,
    ) -> TransactionBatch<'a, 'a> {
        let tx_account_lock_limit = self.get_transaction_account_lock_limit();
        let lock_result = transaction
            .get_account_locks(tx_account_lock_limit)
            .map(|_| ());
        let mut batch = TransactionBatch::new(
            vec![lock_result],
            self,
            Cow::Borrowed(slice::from_ref(transaction)),
        );
        batch.set_needs_unlock(false);
        batch
    }

    pub fn is_frozen(&self) -> bool {
        false
    }

    pub fn freeze_started(&self) -> bool {
        false
    }

    pub fn parent(&self) -> Option<Arc<Bank>> {
        None
    }
    // -----------------
    // Signature Status
    // -----------------
    pub fn get_signature_status(
        &self,
        signature: &Signature,
    ) -> Option<Result<()>> {
        let rcache = self.status_cache.read().unwrap();
        rcache
            .get_recent_transaction_status(signature, None)
            .map(|v| v.1)
    }

    pub fn get_recent_signature_status(
        &self,
        signature: &Signature,
        lookback_slots: Option<Slot>,
    ) -> Option<(Slot, Result<()>)> {
        self.status_cache
            .read()
            .expect("RwLock status_cache poisoned")
            .get_recent_transaction_status(signature, lookback_slots)
    }

    // -----------------
    // Counters
    // -----------------
    /// Return the accumulated executed transaction count
    pub fn transaction_count(&self) -> u64 {
        self.transaction_count.load(Ordering::Relaxed)
    }

    /// Returns the number of non-vote transactions processed without error
    /// since the most recent boot from snapshot or genesis.
    /// This value is not shared though the network, nor retained
    /// within snapshots, but is preserved in `Bank::new_from_parent`.
    pub fn non_vote_transaction_count_since_restart(&self) -> u64 {
        self.non_vote_transaction_count_since_restart
            .load(Ordering::Relaxed)
    }

    /// Return the transaction count executed only in this bank
    pub fn executed_transaction_count(&self) -> u64 {
        self.transaction_count().saturating_sub(
            self.parent().map_or(0, |parent| parent.transaction_count()),
        )
    }

    pub fn transaction_error_count(&self) -> u64 {
        self.transaction_error_count.load(Ordering::Relaxed)
    }

    pub fn transaction_entries_count(&self) -> u64 {
        self.transaction_entries_count.load(Ordering::Relaxed)
    }

    pub fn transactions_per_entry_max(&self) -> u64 {
        self.transactions_per_entry_max.load(Ordering::Relaxed)
    }

    fn increment_transaction_count(&self, tx_count: u64) {
        self.transaction_count
            .fetch_add(tx_count, Ordering::Relaxed);
    }

    fn increment_non_vote_transaction_count_since_restart(
        &self,
        tx_count: u64,
    ) {
        self.non_vote_transaction_count_since_restart
            .fetch_add(tx_count, Ordering::Relaxed);
    }

    fn increment_signature_count(&self, signature_count: u64) {
        self.signature_count
            .fetch_add(signature_count, Ordering::Relaxed);
    }

    /// Update the accounts data size delta from on-chain events by adding `amount`.
    /// The arithmetic saturates.
    fn update_accounts_data_size_delta_on_chain(&self, amount: i64) {
        if amount == 0 {
            return;
        }

        self.accounts_data_size_delta_on_chain
            .fetch_update(
                Ordering::AcqRel,
                Ordering::Acquire,
                |accounts_data_size_delta_on_chain| {
                    Some(
                        accounts_data_size_delta_on_chain
                            .saturating_add(amount),
                    )
                },
            )
            // SAFETY: unwrap() is safe since our update fn always returns `Some`
            .unwrap();
    }

    /// Update the accounts data size delta from off-chain events by adding `amount`.
    /// The arithmetic saturates.
    fn update_accounts_data_size_delta_off_chain(&self, amount: i64) {
        if amount == 0 {
            return;
        }

        self.accounts_data_size_delta_off_chain
            .fetch_update(
                Ordering::AcqRel,
                Ordering::Acquire,
                |accounts_data_size_delta_off_chain| {
                    Some(
                        accounts_data_size_delta_off_chain
                            .saturating_add(amount),
                    )
                },
            )
            // SAFETY: unwrap() is safe since our update fn always returns `Some`
            .unwrap();
    }

    /// Calculate the data size delta and update the off-chain accounts data size delta
    fn calculate_and_update_accounts_data_size_delta_off_chain(
        &self,
        old_data_size: usize,
        new_data_size: usize,
    ) {
        let data_size_delta =
            calculate_data_size_delta(old_data_size, new_data_size);
        self.update_accounts_data_size_delta_off_chain(data_size_delta);
    }

    // -----------------
    // Health
    // -----------------
    /// Returns true when startup accounts hash verification has completed or never had to run in background.
    pub fn get_startup_verification_complete(&self) -> &Arc<AtomicBool> {
        &self
            .rc
            .accounts
            .accounts_db
            .verify_accounts_hash_in_bg
            .verified
    }

    pub fn set_startup_verification_complete(&self) {
        self.rc
            .accounts
            .accounts_db
            .verify_accounts_hash_in_bg
            .verification_complete()
    }

    // -----------------
    // Accessors
    // -----------------
    pub fn read_cost_tracker(
        &self,
    ) -> LockResult<RwLockReadGuard<CostTracker>> {
        self.cost_tracker.read()
    }

    pub fn write_cost_tracker(
        &self,
    ) -> LockResult<RwLockWriteGuard<CostTracker>> {
        self.cost_tracker.write()
    }

    // NOTE: seems to be a synchronization point, i.e. only one thread can hold this
    // at a time
    pub fn freeze_lock(&self) -> RwLockReadGuard<Hash> {
        self.hash.read().unwrap()
    }

    /// Return the total capitalization of the Bank
    pub fn capitalization(&self) -> u64 {
        self.capitalization.load(Ordering::Relaxed)
    }

    pub fn accounts_db_storage_size(&self) -> AccountsDbResult<u64> {
        self.accounts_db().storage_size()
    }

    fn accounts_db(&self) -> &AccountsDb {
        self.rc.accounts.accounts_db.as_ref()
    }

    // -----------------
    // Utilities
    // -----------------
    pub fn slots_for_duration(&self, duration: Duration) -> Slot {
        duration.as_millis() as u64 / self.millis_per_slot
    }

    // -----------------
    // Ledger Replay
    // -----------------
    pub fn replay_slot(
        &self,
        next_slot: Slot,
        current_hash: &Hash,
        blockhash: &Hash,
        timestamp: u64,
    ) {
        self.set_next_slot(next_slot);

        if next_slot > 0 {
            self.status_cache
                .write()
                .expect("RwLock of status cache poisoned")
                .add_root(next_slot - 1);
        }

        self.update_sysvars(
            self.genesis_creation_time,
            Some(timestamp as UnixTimestamp),
        );

        // Register the new blockhash with the blockhash queue
        self.register_hash(blockhash);

        // NOTE: Not notifying Geyser Service doing replay

        // Update loaded programs cache as otherwise we cannot deploy new programs
        self.sync_loaded_programs_cache_to_slot();

        if next_slot > 0 {
            self.update_slot_hashes_and_slot_history(
                next_slot - 1,
                *current_hash,
            );
        }
    }

    fn register_hash(&self, hash: &Hash) {
        let mut blockhash_queue = self.blockhash_queue.write().unwrap();
        blockhash_queue
            .register_hash(hash, self.fee_rate_governor.lamports_per_signature);
    }

    // -----------------
    // Advance Slot/Replay Slot common methods
    // -----------------
    fn set_next_slot(&self, next_slot: Slot) {
        self.set_slot(next_slot);
        self.rc.accounts.set_slot(next_slot);

        let tx_processor = self.transaction_processor.read().unwrap();
        // Update transaction processor with new slot
        // First create a new transaction processor
        let next_tx_processor: TransactionBatchProcessor<_> =
            tx_processor.new_from(next_slot, self.epoch);
        // Then assign the previous sysvar cache to the new transaction processor
        // in order to avoid it containing uninitialized sysvars
        {
            let mut old_sysvar_cache = tx_processor.sysvar_cache();

            let mut new_sysvar_cache = next_tx_processor.sysvar_cache();

            mem::swap(&mut new_sysvar_cache, &mut old_sysvar_cache);
        }
        // prevent deadlocking
        drop(tx_processor);
        *self
            .transaction_processor
            .write()
            .expect("Transaction processor poisoned") = next_tx_processor;
    }

    // timestamp is only provided when replaying the ledger and is otherwise
    // obtained from the system clock
    fn update_sysvars(
        &self,
        epoch_start_timestamp: UnixTimestamp,
        timestamp: Option<UnixTimestamp>,
    ) {
        self.update_clock(epoch_start_timestamp, timestamp);
        self.fill_missing_sysvar_cache_entries();
    }

    fn update_slot_hashes_and_slot_history(
        &self,
        prev_slot: Slot,
        current_hash: Hash,
    ) {
        // Update slot hashes that are needed to sanitize a transaction in some cases
        // NOTE: slothash and blockhash are the same for us
        //       in solana the blockhash is set to the hash of the slot that is finalized
        self.update_slot_hashes(prev_slot, current_hash);
        self.update_slot_history(prev_slot);
    }

    fn inherit_specially_retained_account_fields(
        &self,
        old_account: &Option<AccountSharedData>,
    ) -> InheritableAccountFields {
        const RENT_UNADJUSTED_INITIAL_BALANCE: u64 = 1;

        (
            old_account
                .as_ref()
                .map(|a| a.lamports())
                .unwrap_or(RENT_UNADJUSTED_INITIAL_BALANCE),
            old_account
                .as_ref()
                .map(|a| a.rent_epoch())
                .unwrap_or(INITIAL_RENT_EPOCH),
        )
    }
}
