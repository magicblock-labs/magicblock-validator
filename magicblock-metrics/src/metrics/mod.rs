use std::sync::Once;

pub use prometheus::HistogramTimer;
use prometheus::{
    Histogram, HistogramOpts, HistogramVec, IntCounter, IntCounterVec,
    IntGauge, IntGaugeVec, Opts, Registry,
};
pub use types::{
    AccountClone, AccountCommit, AccountFetchOrigin, LabelValue, Outcome,
};

mod types;

// -----------------
// Buckets
// -----------------
// Prometheus collects durations in seconds
const MICROS_10_90: [f64; 9] = [
    0.000_01, 0.000_02, 0.000_03, 0.000_04, 0.000_05, 0.000_06, 0.000_07,
    0.000_08, 0.000_09,
];
const MICROS_100_900: [f64; 9] = [
    0.000_1, 0.000_2, 0.000_3, 0.000_4, 0.000_5, 0.000_6, 0.000_7, 0.000_8,
    0.000_9,
];
const MILLIS_1_9: [f64; 9] = [
    0.001, 0.002, 0.003, 0.004, 0.005, 0.006, 0.007, 0.008, 0.009,
];
const MILLIS_10_90: [f64; 9] =
    [0.01, 0.02, 0.03, 0.04, 0.05, 0.06, 0.07, 0.08, 0.09];
const MILLIS_100_900: [f64; 9] = [0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9];
const SECONDS_1_9: [f64; 9] = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0];

lazy_static::lazy_static! {
    pub (crate) static ref REGISTRY: Registry = Registry::new_custom(Some("mbv".to_string()), None).unwrap();

    static ref SLOT_COUNT: IntCounter = IntCounter::new(
        "slot_count", "Slot Count",
    ).unwrap();


    static ref CACHED_CLONE_OUTPUTS_COUNT: IntGauge = IntGauge::new(
        "magicblock_account_cloner_cached_outputs_count",
        "Number of cloned accounts in the RemoteAccountClonerWorker"
    )
    .unwrap();

    // -----------------
    // Ledger
    // -----------------
    static ref LEDGER_SIZE_GAUGE: IntGauge = IntGauge::new(
        "ledger_size_gauge", "Ledger size in Bytes",
    ).unwrap();
    static ref LEDGER_BLOCK_TIMES_GAUGE: IntGauge = IntGauge::new(
        "ledger_blocktimes_gauge", "Ledger Blocktimes Gauge",
    ).unwrap();
    static ref LEDGER_BLOCKHASHES_GAUGE: IntGauge = IntGauge::new(
        "ledger_blockhashes_gauge", "Ledger Blockhashes Gauge",
    ).unwrap();
    static ref LEDGER_SLOT_SIGNATURES_GAUGE: IntGauge = IntGauge::new(
        "ledger_slot_signatures_gauge", "Ledger Slot Signatures Gauge",
    ).unwrap();
    static ref LEDGER_ADDRESS_SIGNATURES_GAUGE: IntGauge = IntGauge::new(
        "ledger_address_signatures_gauge", "Ledger Address Signatures Gauge",
    ).unwrap();
    static ref LEDGER_TRANSACTION_STATUS_GAUGE: IntGauge = IntGauge::new(
        "ledger_transaction_status_gauge", "Ledger Transaction Status Gauge",
    ).unwrap();
    static ref LEDGER_TRANSACTION_SUCCESSFUL_STATUS_GAUGE: IntGauge = IntGauge::new(
        "ledger_transaction_successful_status_gauge", "Ledger Successful Transaction Status Gauge",
    ).unwrap();
    static ref LEDGER_TRANSACTION_FAILED_STATUS_GAUGE: IntGauge = IntGauge::new(
        "ledger_transaction_failed_status_gauge", "Ledger Failed Transaction Status Gauge",
    ).unwrap();
    static ref LEDGER_TRANSACTIONS_GAUGE: IntGauge = IntGauge::new(
        "ledger_transactions_gauge", "Ledger Transactions Gauge",
    ).unwrap();
    static ref LEDGER_TRANSACTION_MEMOS_GAUGE: IntGauge = IntGauge::new(
        "ledger_transaction_memos_gauge", "Ledger Transaction Memos Gauge",
    ).unwrap();
    static ref LEDGER_PERF_SAMPLES_GAUGE: IntGauge = IntGauge::new(
        "ledger_perf_samples_gauge", "Ledger Perf Samples Gauge",
    ).unwrap();
    static ref LEDGER_ACCOUNT_MOD_DATA_GAUGE: IntGauge = IntGauge::new(
        "ledger_account_mod_data_gauge", "Ledger Account Mod Data Gauge",
    ).unwrap();
    pub static ref LEDGER_COLUMNS_COUNT_DURATION_SECONDS: Histogram = Histogram::with_opts(
        HistogramOpts::new(
            "ledger_columns_count_duration_seconds",
            "Time taken to compute ledger columns counts"
        )
        .buckets(
            MICROS_10_90.iter().chain(
            MICROS_100_900.iter()).chain(
            MILLIS_1_9.iter()).chain(
            MILLIS_10_90.iter()).chain(
            MILLIS_100_900.iter()).chain(
            SECONDS_1_9.iter()).cloned().collect()
        ),
    ).unwrap();

    // -----------------
    // Accounts
    // -----------------
    static ref ACCOUNTS_SIZE_GAUGE: IntGauge = IntGauge::new(
        "accounts_size_gauge", "Size of persisted accounts (in bytes) currently on disk",
    ).unwrap();

    static ref ACCOUNTS_COUNT_GAUGE: IntGauge = IntGauge::new(
        "accounts_count_gauge", "Number of accounts currently in the database",
    ).unwrap();


    static ref PENDING_ACCOUNT_CLONES_GAUGE: IntGauge = IntGauge::new(
        "pending_account_clones_gauge", "Total number of account clone requests still in memory",
    ).unwrap();

    static ref MONITORED_ACCOUNTS_GAUGE: IntGauge = IntGauge::new(
        "monitored_accounts_gauge", "number of undelegated accounts, being monitored via websocket",
    ).unwrap();

    static ref EVICTED_ACCOUNTS_COUNT: IntCounter = IntCounter::new(
        "evicted_accounts_count", "Total cumulative number of accounts forcefully removed from monitored list and database (monotonically increasing)",
    ).unwrap();

    // -----------------
    // RPC/Aperture
    // -----------------
    pub static ref ENSURE_ACCOUNTS_TIME: HistogramVec = HistogramVec::new(
        HistogramOpts::new("ensure_accounts_time", "Time spent in ensuring account presence")
            .buckets(
                MICROS_10_90.iter().chain(
                MICROS_100_900.iter()).chain(
                MILLIS_1_9.iter()).chain(
                MILLIS_10_90.iter()).chain(
                MILLIS_100_900.iter()).chain(
                SECONDS_1_9.iter()).cloned().collect()
            ),
        &["kind"]
    ).unwrap();

    pub static ref TRANSACTION_PROCESSING_TIME: Histogram = Histogram::with_opts(
        HistogramOpts::new("transaction_processing_time", "Total time spent in transaction processing")
            .buckets(
                MICROS_10_90.iter().chain(
                MICROS_100_900.iter()).chain(
                MILLIS_1_9.iter()).chain(
                MILLIS_10_90.iter()).chain(
                MILLIS_100_900.iter()).chain(
                SECONDS_1_9.iter()).cloned().collect()
            ),
    ).unwrap();

    pub static ref RPC_REQUEST_HANDLING_TIME: HistogramVec = HistogramVec::new(
        HistogramOpts::new("rpc_request_handling_time", "Time spent in rpc request handling")
            .buckets(
                MICROS_10_90.iter().chain(
                MICROS_100_900.iter()).chain(
                MILLIS_1_9.iter()).chain(
                MILLIS_10_90.iter()).chain(
                MILLIS_100_900.iter()).chain(
                SECONDS_1_9.iter()).cloned().collect()
            ), &["name"]
    ).unwrap();

    pub static ref TRANSACTION_SKIP_PREFLIGHT: IntCounter = IntCounter::new(
        "transaction_skip_preflight_count", "Count of transactions that skipped the preflight check",
    ).unwrap();

    pub static ref RPC_REQUESTS_COUNT: IntCounterVec = IntCounterVec::new(
        Opts::new("rpc_requests_count", "Count of different rpc requests"),
        &["name"],
    ).unwrap();

    pub static ref RPC_WS_SUBSCRIPTIONS_COUNT: IntGaugeVec = IntGaugeVec::new(
        Opts::new("rpc_ws_subscriptions_count", "Count of active rpc websocket subscriptions"),
        &["name"],
    ).unwrap();

    // Account fetch results from network (RPC)
    pub static ref ACCOUNT_FETCHES_SUCCESS_COUNT: IntCounter =
        IntCounter::new(
            "account_fetches_success_count",
            "Total number of successful network \
             account fetches",
        )
        .unwrap();

    pub static ref ACCOUNT_FETCHES_FAILED_COUNT: IntCounter =
        IntCounter::new(
            "account_fetches_failed_count",
            "Total number of failed network account fetches \
             (RPC errors)",
        )
        .unwrap();

    pub static ref ACCOUNT_FETCHES_FOUND_COUNT: IntCounterVec = IntCounterVec::new(
        Opts::new(
            "account_fetches_found_count",
            "Total number of network account fetches that found an account",
        ),
        &["origin"],
    )
    .unwrap();

    pub static ref ACCOUNT_FETCHES_NOT_FOUND_COUNT: IntCounterVec = IntCounterVec::new(
        Opts::new(
            "account_fetches_not_found_count",
            "Total number of network account fetches where account was not found",
        ),
        &["origin"],
    )
    .unwrap();

    pub static ref UNDELEGATION_REQUESTED_COUNT: IntCounter =
        IntCounter::new(
            "undelegation_requested_count",
            "Total number of undelegation requests received",
        )
        .unwrap();

    pub static ref UNDELEGATION_COMPLETED_COUNT: IntCounter =
        IntCounter::new(
            "undelegation_completed_count",
            "Total number of completed undelegations detected",
        )
        .unwrap();

    pub static ref UNSTUCK_UNDELEGATION_COUNT: IntCounter =
        IntCounter::new(
            "unstuck_undelegation_count",
            "Total number of undelegating accounts found to be already undelegated on chain",
        )
        .unwrap();


    // -----------------
    // Transaction Execution
    // -----------------
    pub static ref TRANSACTION_COUNT: IntCounter = IntCounter::new(
        "transaction_count", "Total number of executed transactions"
    ).unwrap();

    pub static ref FAILED_TRANSACTIONS_COUNT: IntCounter = IntCounter::new(
        "failed_transactions_count", "Total number of failed transactions"
    ).unwrap();


    // -----------------
    // CommittorService
    // -----------------
    static ref COMMITTOR_INTENTS_COUNT: IntCounter = IntCounter::new(
        "committor_intents_count", "Total number of scheduled committor intents"
    ).unwrap();

    static ref COMMITTOR_INTENTS_BACKLOG_COUNT: IntGauge = IntGauge::new(
        "committor_intent_backlog_count", "Number of intents in backlog",
    ).unwrap();

    static ref COMMITTOR_FAILED_INTENTS_COUNT: IntCounterVec = IntCounterVec::new(
        Opts::new("committor_failed_intents_count", "Number of failed to be executed intents"),
        &["intent_kind", "error_kind"]
    ).unwrap();

    static ref COMMITTOR_EXECUTORS_BUSY_COUNT: IntGauge = IntGauge::new(
        "committor_executors_busy_count", "Number of busy intent executors"
    ).unwrap();

    static ref COMMITTOR_INTENT_EXECUTION_TIME_HISTOGRAM: HistogramVec = HistogramVec::new(
        HistogramOpts::new(
            "committor_intent_execution_time_histogram_v2",
            "Time in seconds spent on intent execution"
        )
        .buckets(
            vec![0.01, 0.1, 1.0, 3.0, 5.0, 10.0, 15.0, 20.0, 25.0]
        ),
        &["intent_kind", "outcome_kind"],
    ).unwrap();

    static ref COMMITTOR_INTENT_CU_USAGE: IntGauge = IntGauge::new(
        "committor_intent_cu_usage_gauge", "Compute units used for Intent"
    ).unwrap();

    // GetMultiplAccount investigation
    static ref REMOTE_ACCOUNT_PROVIDER_A_COUNT: IntCounter = IntCounter::new(
        "remote_account_provider_a_count", "Get mupltiple account count"
    ).unwrap();

    static ref TASK_INFO_FETCHER_A_COUNT: IntCounter = IntCounter::new(
        "task_info_fetcher_a_count", "Get mupltiple account count"
    ).unwrap();

    static ref TASK_INFO_FETCHER_B_COUNT: IntCounter = IntCounter::new(
        "task_info_fetcher_b_count", "Get multiple compressed delegation records count"
    ).unwrap();

    static ref TABLE_MANIA_A_COUNT: IntCounter =  IntCounter::new(
        "table_mania_a_count", "Get mupltiple account count"
    ).unwrap();

    static ref TABLE_MANIA_CLOSED_A_COUNT: IntCounter = IntCounter::new(
        "table_mania_closed_a_count", "Get account counter"
    ).unwrap();


    static ref COMMITTOR_INTENT_TASK_PREPARATION_TIME: HistogramVec = HistogramVec::new(
        HistogramOpts::new(
            "committor_intent_task_preparation_time",
            "Time in seconds spent on task preparation"
        )
        .buckets(
            vec![0.1, 1.0, 2.0, 3.0, 5.0]
        ),
        &["task_type"],
    ).unwrap();

    static ref COMMITTOR_INTENT_ALT_PREPARATION_TIME: Histogram = Histogram::with_opts(
        HistogramOpts::new(
            "committor_intent_alt_preparation_time",
            "Time in seconds spent on ALTs preparation"
        )
        .buckets(
            vec![1.0, 3.0, 5.0, 10.0, 15.0, 17.0, 20.0]
        ),
    ).unwrap();
}

pub(crate) fn register() {
    static REGISTER: Once = Once::new();
    REGISTER.call_once(|| {
        macro_rules! register {
            ($collector:ident) => {
                REGISTRY
                    .register(Box::new($collector.clone()))
                    .expect("collector can't be registered");
            };
        }
        register!(SLOT_COUNT);
        register!(CACHED_CLONE_OUTPUTS_COUNT);
        register!(LEDGER_SIZE_GAUGE);
        register!(LEDGER_BLOCK_TIMES_GAUGE);
        register!(LEDGER_BLOCKHASHES_GAUGE);
        register!(LEDGER_SLOT_SIGNATURES_GAUGE);
        register!(LEDGER_ADDRESS_SIGNATURES_GAUGE);
        register!(LEDGER_TRANSACTION_STATUS_GAUGE);
        register!(LEDGER_TRANSACTION_SUCCESSFUL_STATUS_GAUGE);
        register!(LEDGER_TRANSACTION_FAILED_STATUS_GAUGE);
        register!(LEDGER_TRANSACTIONS_GAUGE);
        register!(LEDGER_TRANSACTION_MEMOS_GAUGE);
        register!(LEDGER_PERF_SAMPLES_GAUGE);
        register!(LEDGER_ACCOUNT_MOD_DATA_GAUGE);
        register!(LEDGER_COLUMNS_COUNT_DURATION_SECONDS);
        register!(ACCOUNTS_SIZE_GAUGE);
        register!(ACCOUNTS_COUNT_GAUGE);
        register!(PENDING_ACCOUNT_CLONES_GAUGE);
        register!(MONITORED_ACCOUNTS_GAUGE);
        register!(EVICTED_ACCOUNTS_COUNT);
        register!(COMMITTOR_INTENTS_COUNT);
        register!(COMMITTOR_INTENTS_BACKLOG_COUNT);
        register!(COMMITTOR_FAILED_INTENTS_COUNT);
        register!(COMMITTOR_EXECUTORS_BUSY_COUNT);
        register!(COMMITTOR_INTENT_EXECUTION_TIME_HISTOGRAM);
        register!(COMMITTOR_INTENT_CU_USAGE);
        register!(COMMITTOR_INTENT_TASK_PREPARATION_TIME);
        register!(COMMITTOR_INTENT_ALT_PREPARATION_TIME);
        register!(ENSURE_ACCOUNTS_TIME);
        register!(RPC_REQUEST_HANDLING_TIME);
        register!(TRANSACTION_PROCESSING_TIME);
        register!(TRANSACTION_SKIP_PREFLIGHT);
        register!(RPC_REQUESTS_COUNT);
        register!(RPC_WS_SUBSCRIPTIONS_COUNT);
        register!(ACCOUNT_FETCHES_SUCCESS_COUNT);
        register!(ACCOUNT_FETCHES_FAILED_COUNT);
        register!(ACCOUNT_FETCHES_FOUND_COUNT);
        register!(ACCOUNT_FETCHES_NOT_FOUND_COUNT);
        register!(UNDELEGATION_REQUESTED_COUNT);
        register!(UNDELEGATION_COMPLETED_COUNT);
        register!(UNSTUCK_UNDELEGATION_COUNT);
        register!(FAILED_TRANSACTIONS_COUNT);
        register!(REMOTE_ACCOUNT_PROVIDER_A_COUNT);
        register!(TASK_INFO_FETCHER_A_COUNT);
        register!(TASK_INFO_FETCHER_B_COUNT);
        register!(TABLE_MANIA_A_COUNT);
        register!(TABLE_MANIA_CLOSED_A_COUNT);
    });
}

pub fn inc_slot() {
    SLOT_COUNT.inc();
}

pub fn set_cached_clone_outputs_count(count: usize) {
    CACHED_CLONE_OUTPUTS_COUNT.set(count as i64);
}

pub fn set_ledger_size(size: u64) {
    LEDGER_SIZE_GAUGE.set(size as i64);
}

pub fn set_ledger_block_times_count(count: i64) {
    LEDGER_BLOCK_TIMES_GAUGE.set(count);
}

pub fn set_ledger_blockhashes_count(count: i64) {
    LEDGER_BLOCKHASHES_GAUGE.set(count);
}

pub fn set_ledger_slot_signatures_count(count: i64) {
    LEDGER_SLOT_SIGNATURES_GAUGE.set(count);
}

pub fn set_ledger_address_signatures_count(count: i64) {
    LEDGER_ADDRESS_SIGNATURES_GAUGE.set(count);
}

pub fn set_ledger_transaction_status_count(count: i64) {
    LEDGER_TRANSACTION_STATUS_GAUGE.set(count);
}

pub fn set_ledger_transaction_successful_status_count(count: i64) {
    LEDGER_TRANSACTION_SUCCESSFUL_STATUS_GAUGE.set(count);
}

pub fn set_ledger_transaction_failed_status_count(count: i64) {
    LEDGER_TRANSACTION_FAILED_STATUS_GAUGE.set(count);
}

pub fn set_ledger_transactions_count(count: i64) {
    LEDGER_TRANSACTIONS_GAUGE.set(count);
}

pub fn set_ledger_transaction_memos_count(count: i64) {
    LEDGER_TRANSACTION_MEMOS_GAUGE.set(count);
}

pub fn set_ledger_perf_samples_count(count: i64) {
    LEDGER_PERF_SAMPLES_GAUGE.set(count);
}

pub fn set_ledger_account_mod_data_count(count: i64) {
    LEDGER_ACCOUNT_MOD_DATA_GAUGE.set(count);
}

pub fn observe_columns_count_duration<F, T>(f: F) -> T
where
    F: FnOnce() -> T,
{
    LEDGER_COLUMNS_COUNT_DURATION_SECONDS.observe_closure_duration(f)
}

pub fn set_accounts_size(value: i64) {
    ACCOUNTS_SIZE_GAUGE.set(value)
}

pub fn set_accounts_count(value: i64) {
    ACCOUNTS_COUNT_GAUGE.set(value)
}

pub fn inc_pending_clone_requests() {
    PENDING_ACCOUNT_CLONES_GAUGE.inc()
}

pub fn dec_pending_clone_requests() {
    PENDING_ACCOUNT_CLONES_GAUGE.dec()
}

pub fn ensure_accounts_end(timer: HistogramTimer) {
    timer.stop_and_record();
}

/// Sets the absolute number of monitored accounts.
///
/// This metric reflects the current total count of accounts being monitored.
/// Callers must pass the total number of monitored accounts, not a delta.
pub fn set_monitored_accounts_count(count: usize) {
    MONITORED_ACCOUNTS_GAUGE.set(count as i64);
}
pub fn inc_evicted_accounts_count() {
    EVICTED_ACCOUNTS_COUNT.inc();
}

pub fn inc_committor_intents_count() {
    COMMITTOR_INTENTS_COUNT.inc()
}

pub fn inc_committor_intents_count_by(by: u64) {
    COMMITTOR_INTENTS_COUNT.inc_by(by)
}

pub fn set_committor_intents_backlog_count(value: i64) {
    COMMITTOR_INTENTS_BACKLOG_COUNT.set(value)
}

pub fn inc_committor_failed_intents_count(
    intent_kind: &impl LabelValue,
    error_kind: &impl LabelValue,
) {
    COMMITTOR_FAILED_INTENTS_COUNT
        .with_label_values(&[intent_kind.value(), error_kind.value()])
        .inc()
}

pub fn set_committor_executors_busy_count(value: i64) {
    COMMITTOR_EXECUTORS_BUSY_COUNT.set(value)
}

pub fn observe_committor_intent_execution_time_histogram(
    seconds: f64,
    kind: &impl LabelValue,
    outcome: &impl LabelValue,
) {
    COMMITTOR_INTENT_EXECUTION_TIME_HISTOGRAM
        .with_label_values(&[kind.value(), outcome.value()])
        .observe(seconds);
}

pub fn set_commmittor_intent_cu_usage(value: i64) {
    COMMITTOR_INTENT_CU_USAGE.set(value)
}

pub fn observe_committor_intent_task_preparation_time<
    L: LabelValue + ?Sized,
>(
    task_type: &L,
) -> HistogramTimer {
    COMMITTOR_INTENT_TASK_PREPARATION_TIME
        .with_label_values(&[task_type.value()])
        .start_timer()
}

pub fn observe_committor_intent_alt_preparation_time() -> HistogramTimer {
    COMMITTOR_INTENT_ALT_PREPARATION_TIME.start_timer()
}

pub fn inc_account_fetches_success(count: u64) {
    ACCOUNT_FETCHES_SUCCESS_COUNT.inc_by(count);
}

pub fn inc_account_fetches_failed(count: u64) {
    ACCOUNT_FETCHES_FAILED_COUNT.inc_by(count);
}

pub fn inc_account_fetches_found(fetch_origin: AccountFetchOrigin, count: u64) {
    ACCOUNT_FETCHES_FOUND_COUNT
        .with_label_values(&[fetch_origin.value()])
        .inc_by(count);
}

pub fn inc_account_fetches_not_found(
    fetch_origin: AccountFetchOrigin,
    count: u64,
) {
    ACCOUNT_FETCHES_NOT_FOUND_COUNT
        .with_label_values(&[fetch_origin.value()])
        .inc_by(count);
}

pub fn inc_undelegation_requested() {
    UNDELEGATION_REQUESTED_COUNT.inc();
}

pub fn inc_undelegation_completed() {
    UNDELEGATION_COMPLETED_COUNT.inc();
}

pub fn inc_unstuck_undelegation_count() {
    UNSTUCK_UNDELEGATION_COUNT.inc();
}

pub fn inc_remote_account_provider_a_count() {
    REMOTE_ACCOUNT_PROVIDER_A_COUNT.inc()
}

pub fn inc_task_info_fetcher_a_count() {
    TASK_INFO_FETCHER_A_COUNT.inc()
}

pub fn inc_task_info_fetcher_b_count() {
    TASK_INFO_FETCHER_B_COUNT.inc()
}

pub fn inc_table_mania_a_count() {
    TABLE_MANIA_A_COUNT.inc()
}

pub fn inc_table_mania_close_a_count() {
    TABLE_MANIA_CLOSED_A_COUNT.inc()
}
