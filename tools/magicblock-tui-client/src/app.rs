use std::{
    io::{self, Stdout},
    panic,
    time::Duration,
};

use chrono::Local;
use crossterm::{
    cursor,
    event::Event,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use futures_util::StreamExt;
use ratatui::{backend::CrosstermBackend, Terminal};
use serde::Deserialize;
use solana_pubsub_client::nonblocking::pubsub_client::PubsubClient;
use solana_rpc_client_api::{
    config::{RpcTransactionLogsConfig, RpcTransactionLogsFilter},
    custom_error::{
        JSON_RPC_SERVER_ERROR_BLOCK_CLEANED_UP,
        JSON_RPC_SERVER_ERROR_BLOCK_NOT_AVAILABLE,
        JSON_RPC_SERVER_ERROR_BLOCK_STATUS_NOT_AVAILABLE_YET,
        JSON_RPC_SERVER_ERROR_LONG_TERM_STORAGE_SLOT_SKIPPED,
        JSON_RPC_SERVER_ERROR_SLOT_SKIPPED,
        JSON_RPC_SERVER_ERROR_TRANSACTION_HISTORY_NOT_AVAILABLE,
    },
};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio_util::sync::CancellationToken;
use tracing::Level;

use crate::{
    events::{handle_event, poll_event, EventAction},
    state::{
        LogEntry, TransactionAccount, TransactionDetail, TransactionEntry,
        TransactionSource, TuiConfig, TuiState,
    },
    ui,
    utils::{url_encode, websocket_url_from_rpc_url},
};

type Term = Terminal<CrosstermBackend<Stdout>>;
const VOTE_PROGRAM_ID: &str = "Vote111111111111111111111111111111111111111";
const BLOCK_FEED_CONFIRMATION_LAG_SLOTS: u64 = 0;
const INITIAL_TRANSACTION_BACKFILL_SLOTS: u64 = 64;
const BLOCK_FEED_RETRY_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Debug)]
enum AppEvent {
    Slot(u64),
    Transaction(TransactionSource, TransactionEntry),
    TransactionDetail(TransactionDetail),
    Log(LogEntry),
}

pub async fn run_tui(config: TuiConfig) -> io::Result<()> {
    let cancel = CancellationToken::new();
    setup_panic_hook();
    let mut terminal = init_terminal()?;
    let mut state = TuiState::new(config.clone());
    let local_ws_url = config.ws_url.replace("0.0.0.0", "localhost");
    let client = reqwest::Client::new();
    if let Ok(epoch_info) = get_epoch_info(&client, &config.rpc_url).await {
        if epoch_info.slots_in_epoch > 0 {
            state.slots_per_epoch = epoch_info.slots_in_epoch;
        }
    }

    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let (slot_tx, slot_rx) = mpsc::unbounded_channel();
    spawn_slot_subscription(
        local_ws_url.clone(),
        "slot_subscribe",
        true,
        event_tx.clone(),
        vec![slot_tx],
        cancel.clone(),
    );
    spawn_block_transaction_feed(
        state.rpc_url.clone(),
        TransactionSource::Local,
        "tx_feed",
        slot_rx,
        event_tx.clone(),
        cancel.clone(),
    );
    if let Some(remote_rpc_url) = state.remote_rpc_url.clone() {
        let (remote_slot_tx, remote_slot_rx) = mpsc::unbounded_channel();
        if let Some(remote_ws_url) = websocket_url_from_rpc_url(&remote_rpc_url)
        {
            spawn_slot_subscription(
                remote_ws_url,
                "remote_slot_subscribe",
                false,
                event_tx.clone(),
                vec![remote_slot_tx],
                cancel.clone(),
            );
            spawn_block_transaction_feed(
                remote_rpc_url,
                TransactionSource::Remote,
                "remote_tx_feed",
                remote_slot_rx,
                event_tx.clone(),
                cancel.clone(),
            );
        } else {
            let _ = event_tx.send(AppEvent::Log(LogEntry::new(
                Level::WARN,
                "remote_slot_subscribe".to_string(),
                format!(
                    "Could not derive websocket endpoint from remote RPC {}",
                    remote_rpc_url
                ),
            )));
        }
    }
    spawn_logs_subscription(local_ws_url, event_tx.clone(), cancel.clone());

    let result = run_event_loop(
        &mut terminal,
        &mut state,
        event_rx,
        event_tx.clone(),
        cancel,
    )
    .await;
    restore_terminal(&mut terminal)?;
    result
}

pub async fn enrich_config_from_rpc(config: &mut TuiConfig) {
    let client = reqwest::Client::new();

    if config.validator_identity.is_empty() {
        if let Ok(identity) = get_identity(&client, &config.rpc_url).await {
            config.validator_identity = identity;
        }
    }

    if let Ok(server_version) =
        get_server_version(&client, &config.rpc_url).await
    {
        config.version =
            format!("{} | validator {}", config.version, server_version);
    }
}

fn setup_panic_hook() {
    let original_hook = panic::take_hook();
    panic::set_hook(Box::new(move |panic_info| {
        let _ = terminal::disable_raw_mode();
        let _ = io::stdout().execute(LeaveAlternateScreen);
        let _ = io::stdout().execute(cursor::Show);
        original_hook(panic_info);
    }));
}

fn init_terminal() -> io::Result<Term> {
    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    stdout.execute(cursor::Hide)?;
    Terminal::new(CrosstermBackend::new(stdout))
}

fn restore_terminal(terminal: &mut Term) -> io::Result<()> {
    terminal::disable_raw_mode()?;
    terminal.backend_mut().execute(LeaveAlternateScreen)?;
    terminal.backend_mut().execute(cursor::Show)?;
    Ok(())
}

async fn run_event_loop(
    terminal: &mut Term,
    state: &mut TuiState,
    mut event_rx: UnboundedReceiver<AppEvent>,
    event_tx: UnboundedSender<AppEvent>,
    cancel: CancellationToken,
) -> io::Result<()> {
    let rpc_client = reqwest::Client::new();
    let poll_timeout = Duration::ZERO;
    let tick_rate = Duration::from_millis(50);
    let mut last_tick = std::time::Instant::now();

    terminal.draw(|f| ui::render(f, state))?;

    loop {
        if cancel.is_cancelled() {
            state.should_quit = true;
        }

        if state.should_quit {
            cancel.cancel();
            break;
        }

        if let Some(event) = poll_event(poll_timeout) {
            let is_resize = matches!(event, Event::Resize(_, _));
            let terminal_area = terminal
                .size()
                .map(|size| {
                    ratatui::layout::Rect::new(0, 0, size.width, size.height)
                })
                .unwrap_or_default();

            let action = handle_event(state, event, terminal_area);
            match action {
                EventAction::FetchTransaction { rpc_url, signature } => {
                    let client = rpc_client.clone();
                    let event_tx = event_tx.clone();

                    tokio::spawn(async move {
                        let detail = match fetch_transaction_detail(
                            &client, &rpc_url, &signature,
                        )
                        .await
                        {
                            Ok(detail) => detail,
                            Err(e) => build_failed_tx_detail(
                                &rpc_url,
                                signature,
                                format!("Failed to fetch: {}", e),
                            ),
                        };
                        let _ =
                            event_tx.send(AppEvent::TransactionDetail(detail));
                    });
                }
                EventAction::OpenUrl(url) => {
                    let _ = open_url_in_browser(&url);
                }
                EventAction::None => {}
            }

            if is_resize {
                terminal.draw(|f| ui::render(f, state))?;
                continue;
            }
        }

        while let Ok(event) = event_rx.try_recv() {
            match event {
                AppEvent::Slot(slot) => state.update_slot(slot),
                AppEvent::Transaction(source, tx) => {
                    state.push_transaction(source, tx)
                }
                AppEvent::TransactionDetail(detail) => {
                    state.show_tx_detail(detail)
                }
                AppEvent::Log(log) => state.push_log(log),
            }
        }

        if last_tick.elapsed() >= tick_rate {
            terminal.draw(|f| ui::render(f, state))?;
            last_tick = std::time::Instant::now();
        }

        tokio::time::sleep(Duration::from_millis(1)).await;
    }

    Ok(())
}

fn spawn_slot_subscription(
    ws_url: String,
    log_target: &'static str,
    forward_slot_to_state: bool,
    event_tx: UnboundedSender<AppEvent>,
    slot_txs: Vec<UnboundedSender<u64>>,
    cancel: CancellationToken,
) {
    tokio::spawn(async move {
        while !cancel.is_cancelled() {
            match PubsubClient::new(&ws_url).await {
                Ok(client) => {
                    let _ = event_tx.send(AppEvent::Log(LogEntry::new(
                        Level::INFO,
                        log_target.to_string(),
                        format!("Connected to {}", ws_url),
                    )));

                    match client.slot_subscribe().await {
                        Ok((mut stream, unsubscribe)) => {
                            loop {
                                tokio::select! {
                                    _ = cancel.cancelled() => break,
                                    item = stream.next() => {
                                        match item {
                                            Some(update) => {
                                                let slot = update.slot;
                                                if forward_slot_to_state {
                                                    let _ = event_tx.send(AppEvent::Slot(slot));
                                                }
                                                for slot_tx in &slot_txs {
                                                    let _ = slot_tx.send(slot);
                                                }
                                            }
                                            None => break,
                                        }
                                    }
                                }
                            }
                            let _ = unsubscribe().await;
                        }
                        Err(err) => {
                            let _ =
                                event_tx.send(AppEvent::Log(LogEntry::new(
                                    Level::ERROR,
                                    log_target.to_string(),
                                    format!("Subscription failed: {}", err),
                                )));
                        }
                    }
                }
                Err(err) => {
                    let _ = event_tx.send(AppEvent::Log(LogEntry::new(
                        Level::ERROR,
                        log_target.to_string(),
                        format!("Connection failed: {}", err),
                    )));
                }
            }

            if !cancel.is_cancelled() {
                tokio::time::sleep(Duration::from_millis(800)).await;
            }
        }
    });
}

fn spawn_block_transaction_feed(
    rpc_url: String,
    source: TransactionSource,
    log_target: &'static str,
    mut slot_rx: UnboundedReceiver<u64>,
    event_tx: UnboundedSender<AppEvent>,
    cancel: CancellationToken,
) {
    tokio::spawn(async move {
        let client = reqwest::Client::new();
        let mut latest_target_slot = initialize_transaction_backfill(
            &client, &rpc_url, log_target, &event_tx,
        )
        .await;
        let mut next_slot_to_fetch =
            latest_target_slot.map(backfill_start_slot);

        let _ = event_tx.send(AppEvent::Log(LogEntry::new(
            Level::INFO,
            log_target.to_string(),
            format!("Using getBlock transaction feed on {}", rpc_url),
        )));

        while !cancel.is_cancelled() {
            while let (Some(next_slot), Some(latest_slot)) =
                (next_slot_to_fetch, latest_target_slot)
            {
                if next_slot > latest_slot || cancel.is_cancelled() {
                    break;
                }

                let should_advance = match fetch_block_transactions_with_retry(
                    &client,
                    &rpc_url,
                    next_slot,
                    source == TransactionSource::Remote,
                )
                .await
                {
                    Ok((BlockFetchOutcome::Entries, entries)) => {
                        for entry in entries {
                            let _ = event_tx
                                .send(AppEvent::Transaction(source, entry));
                        }
                        true
                    }
                    Ok((BlockFetchOutcome::SkipSlot, _)) => true,
                    Ok((BlockFetchOutcome::RetryLater, _)) => false,
                    Err(err) => {
                        let _ = event_tx.send(AppEvent::Log(LogEntry::new(
                            Level::WARN,
                            log_target.to_string(),
                            format!(
                                "getBlock slot {} failed: {}",
                                next_slot, err
                            ),
                        )));
                        false
                    }
                };

                if should_advance {
                    next_slot_to_fetch = Some(next_slot.saturating_add(1));
                } else {
                    break;
                }
            }

            tokio::select! {
                _ = cancel.cancelled() => break,
                maybe_slot = slot_rx.recv() => {
                    let slot = match maybe_slot {
                        Some(slot) => slot,
                        None => break,
                    };

                    let target_slot = live_feed_target_slot(slot);
                    latest_target_slot = Some(match latest_target_slot {
                        Some(current) => current.max(target_slot),
                        None => target_slot,
                    });

                    if next_slot_to_fetch.is_none() {
                        next_slot_to_fetch =
                            Some(backfill_start_slot(target_slot));
                    }
                }
                _ = tokio::time::sleep(BLOCK_FEED_RETRY_INTERVAL) => {
                    if let Ok(slot) = get_confirmed_slot(&client, &rpc_url).await {
                        latest_target_slot = Some(match latest_target_slot {
                            Some(current) => current.max(slot),
                            None => slot,
                        });

                        if next_slot_to_fetch.is_none() {
                            next_slot_to_fetch =
                                Some(backfill_start_slot(slot));
                        }
                    }
                }
            }
        }
    });
}

fn spawn_logs_subscription(
    ws_url: String,
    event_tx: UnboundedSender<AppEvent>,
    cancel: CancellationToken,
) {
    tokio::spawn(async move {
        while !cancel.is_cancelled() {
            match PubsubClient::new(&ws_url).await {
                Ok(client) => {
                    let _ = event_tx.send(AppEvent::Log(LogEntry::new(
                        Level::INFO,
                        "logs_subscribe".to_string(),
                        format!("Connected to {}", ws_url),
                    )));

                    match client
                        .logs_subscribe(
                            RpcTransactionLogsFilter::All,
                            RpcTransactionLogsConfig { commitment: None },
                        )
                        .await
                    {
                        Ok((mut stream, unsubscribe)) => {
                            loop {
                                tokio::select! {
                                    _ = cancel.cancelled() => break,
                                    item = stream.next() => {
                                        match item {
                                            Some(update) => {
                                                let signature = update.value.signature.clone();
                                                let success = update.value.err.is_none();
                                                let slot = update.context.slot;
                                                let _ = event_tx.send(AppEvent::Transaction(
                                                    TransactionSource::Local,
                                                    TransactionEntry {
                                                        signature: signature.clone(),
                                                        slot,
                                                        success,
                                                        timestamp: Local::now(),
                                                        accounts: vec![],
                                                    },
                                                ));

                                                let level = if success { Level::INFO } else { Level::ERROR };
                                                let summary = if success {
                                                    format!("tx {} succeeded in slot {}", signature, slot)
                                                } else {
                                                    format!("tx {} failed in slot {}: {:?}", signature, slot, update.value.err)
                                                };
                                                let _ = event_tx.send(AppEvent::Log(LogEntry::new(
                                                    level,
                                                    "tx".to_string(),
                                                    summary,
                                                )));

                                                for line in update.value.logs {
                                                    let _ = event_tx.send(AppEvent::Log(LogEntry::new(
                                                        Level::INFO,
                                                        "tx-log".to_string(),
                                                        line,
                                                    )));
                                                }
                                            }
                                            None => break,
                                        }
                                    }
                                }
                            }
                            let _ = unsubscribe().await;
                        }
                        Err(err) => {
                            let _ =
                                event_tx.send(AppEvent::Log(LogEntry::new(
                                    Level::ERROR,
                                    "logs_subscribe".to_string(),
                                    format!("Subscription failed: {}", err),
                                )));
                        }
                    }
                }
                Err(err) => {
                    let _ = event_tx.send(AppEvent::Log(LogEntry::new(
                        Level::ERROR,
                        "logs_subscribe".to_string(),
                        format!("Connection failed: {}", err),
                    )));
                }
            }

            if !cancel.is_cancelled() {
                tokio::time::sleep(Duration::from_millis(800)).await;
            }
        }
    });
}

#[derive(Debug, Deserialize)]
struct RpcResponse<T> {
    result: Option<T>,
    error: Option<RpcError>,
}

#[derive(Debug, Deserialize)]
struct RpcError {
    code: i64,
    message: String,
}

#[derive(Debug, Deserialize)]
struct TransactionResponse {
    slot: u64,
    meta: Option<TransactionMeta>,
    transaction: TransactionData,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BlockResponse {
    #[serde(default)]
    transactions: Vec<BlockTransactionWithMeta>,
}

#[derive(Debug, Deserialize)]
struct BlockTransactionWithMeta {
    transaction: BlockTransaction,
    meta: Option<BlockTransactionMeta>,
}

#[derive(Debug, Deserialize)]
struct BlockTransaction {
    signatures: Vec<String>,
    message: BlockTransactionMessage,
}

#[derive(Debug, Deserialize)]
struct BlockTransactionMessage {
    #[serde(default, rename = "accountKeys")]
    account_keys: Vec<String>,
    #[serde(default)]
    instructions: Vec<CompiledInstruction>,
}

#[derive(Debug, Deserialize)]
struct BlockTransactionMeta {
    err: Option<serde_json::Value>,
    #[serde(rename = "loadedAddresses")]
    loaded_addresses: Option<LoadedAddresses>,
}

#[derive(Debug, Clone, Deserialize)]
struct LoadedAddresses {
    #[serde(default)]
    writable: Vec<String>,
    #[serde(default)]
    readonly: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CompiledInstruction {
    program_id_index: usize,
}

#[derive(Debug, Deserialize, Clone)]
struct TransactionMeta {
    fee: u64,
    err: Option<serde_json::Value>,
    #[serde(rename = "logMessages")]
    log_messages: Option<Vec<String>>,
    #[serde(rename = "computeUnitsConsumed")]
    compute_units_consumed: Option<u64>,
    #[serde(rename = "loadedAddresses")]
    loaded_addresses: Option<LoadedAddresses>,
}

#[derive(Debug, Deserialize)]
struct TransactionData {
    message: TransactionMessage,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TransactionMessage {
    #[serde(default)]
    header: TransactionMessageHeader,
    #[serde(rename = "accountKeys")]
    account_keys: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TransactionMessageHeader {
    num_required_signatures: usize,
    num_readonly_signed_accounts: usize,
    num_readonly_unsigned_accounts: usize,
}

#[derive(Debug, Deserialize)]
struct IdentityResponse {
    identity: String,
}

#[derive(Debug, Deserialize)]
struct VersionResponse {
    #[serde(rename = "solana-core")]
    solana_core: String,
}

#[derive(Debug, Deserialize)]
struct EpochInfoResponse {
    #[serde(rename = "slotsInEpoch")]
    slots_in_epoch: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlockFetchOutcome {
    Entries,
    RetryLater,
    SkipSlot,
}

fn live_feed_target_slot(slot: u64) -> u64 {
    slot.saturating_sub(BLOCK_FEED_CONFIRMATION_LAG_SLOTS)
}

fn backfill_start_slot(latest_slot: u64) -> u64 {
    latest_slot.saturating_sub(INITIAL_TRANSACTION_BACKFILL_SLOTS)
}

async fn initialize_transaction_backfill(
    client: &reqwest::Client,
    rpc_url: &str,
    log_target: &str,
    event_tx: &UnboundedSender<AppEvent>,
) -> Option<u64> {
    match get_confirmed_slot(client, rpc_url).await {
        Ok(slot) => {
            let _ = event_tx.send(AppEvent::Log(LogEntry::new(
                Level::INFO,
                log_target.to_string(),
                format!(
                    "Backfilling recent transactions from slot {} to {}",
                    backfill_start_slot(slot),
                    slot
                ),
            )));
            Some(slot)
        }
        Err(err) => {
            let _ = event_tx.send(AppEvent::Log(LogEntry::new(
                Level::WARN,
                log_target.to_string(),
                format!("Failed to initialize transaction backfill: {}", err),
            )));
            None
        }
    }
}

async fn get_identity(
    client: &reqwest::Client,
    rpc_url: &str,
) -> Result<String, String> {
    let request_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getIdentity",
        "params": []
    });

    let response = client
        .post(rpc_url)
        .timeout(Duration::from_secs(5))
        .json(&request_body)
        .send()
        .await
        .map_err(|e| format!("HTTP error: {}", e))?;

    let rpc_response: RpcResponse<IdentityResponse> = response
        .json()
        .await
        .map_err(|e| format!("Parse error: {}", e))?;

    if let Some(err) = rpc_response.error {
        return Err(err.message);
    }

    rpc_response
        .result
        .map(|v| v.identity)
        .ok_or_else(|| "missing result".to_string())
}

async fn get_server_version(
    client: &reqwest::Client,
    rpc_url: &str,
) -> Result<String, String> {
    let request_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getVersion",
        "params": []
    });

    let response = client
        .post(rpc_url)
        .timeout(Duration::from_secs(5))
        .json(&request_body)
        .send()
        .await
        .map_err(|e| format!("HTTP error: {}", e))?;

    let rpc_response: RpcResponse<VersionResponse> = response
        .json()
        .await
        .map_err(|e| format!("Parse error: {}", e))?;

    if let Some(err) = rpc_response.error {
        return Err(err.message);
    }

    rpc_response
        .result
        .map(|v| v.solana_core)
        .ok_or_else(|| "missing result".to_string())
}

async fn get_epoch_info(
    client: &reqwest::Client,
    rpc_url: &str,
) -> Result<EpochInfoResponse, String> {
    let request_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getEpochInfo",
        "params": []
    });

    let response = client
        .post(rpc_url)
        .timeout(Duration::from_secs(5))
        .json(&request_body)
        .send()
        .await
        .map_err(|e| format!("HTTP error: {}", e))?;

    let rpc_response: RpcResponse<EpochInfoResponse> = response
        .json()
        .await
        .map_err(|e| format!("Parse error: {}", e))?;

    if let Some(err) = rpc_response.error {
        return Err(err.message);
    }

    rpc_response
        .result
        .ok_or_else(|| "missing result".to_string())
}

async fn get_confirmed_slot(
    client: &reqwest::Client,
    rpc_url: &str,
) -> Result<u64, String> {
    let request_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getSlot",
        "params": [{
            "commitment": "confirmed"
        }]
    });

    let response = client
        .post(rpc_url)
        .timeout(Duration::from_secs(5))
        .json(&request_body)
        .send()
        .await
        .map_err(|e| format!("HTTP error: {}", e))?;

    let rpc_response: RpcResponse<u64> = response
        .json()
        .await
        .map_err(|e| format!("Parse error: {}", e))?;

    if let Some(err) = rpc_response.error {
        return Err(err.message);
    }

    rpc_response
        .result
        .ok_or_else(|| "missing result".to_string())
}

async fn fetch_transaction_detail(
    client: &reqwest::Client,
    rpc_url: &str,
    signature: &str,
) -> Result<TransactionDetail, String> {
    let request_body = build_transaction_detail_request(signature);

    let response = client
        .post(rpc_url)
        .timeout(Duration::from_secs(10))
        .json(&request_body)
        .send()
        .await
        .map_err(|e| format!("HTTP error: {}", e))?;

    let rpc_response: RpcResponse<TransactionResponse> = response
        .json()
        .await
        .map_err(|e| format!("Parse error: {}", e))?;

    if let Some(err) = rpc_response.error {
        return Err(err.message);
    }

    let tx = rpc_response
        .result
        .ok_or_else(|| "Transaction not found".to_string())?;
    let TransactionResponse {
        slot,
        meta,
        transaction,
    } = tx;
    let success = meta.as_ref().map(|m| m.err.is_none()).unwrap_or(true);
    let error = meta
        .as_ref()
        .and_then(|m| m.err.as_ref())
        .map(|e| format!("{}", e));
    let accounts = build_transaction_accounts(
        transaction.message,
        meta.as_ref().and_then(|m| m.loaded_addresses.clone()),
    );
    let selected_account = if accounts.is_empty() { None } else { Some(0) };

    Ok(TransactionDetail {
        signature: signature.to_string(),
        slot,
        success,
        fee: meta.as_ref().map(|m| m.fee).unwrap_or(0),
        compute_units: meta.as_ref().and_then(|m| m.compute_units_consumed),
        logs: meta
            .as_ref()
            .and_then(|m| m.log_messages.clone())
            .unwrap_or_default(),
        accounts,
        error,
        rpc_url: rpc_url.to_string(),
        explorer_url: build_explorer_url(rpc_url, signature),
        selected_account,
        detail_scroll: 0,
    })
}

fn build_transaction_detail_request(signature: &str) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getTransaction",
        "params": [
            signature,
            {
                "encoding": "json",
                "commitment": "confirmed",
                "maxSupportedTransactionVersion": 255
            }
        ]
    })
}

fn build_failed_tx_detail(
    rpc_url: &str,
    signature: String,
    error: String,
) -> TransactionDetail {
    let explorer_url = build_explorer_url(rpc_url, &signature);
    TransactionDetail {
        signature,
        slot: 0,
        success: false,
        fee: 0,
        compute_units: None,
        logs: vec![],
        accounts: vec![],
        error: Some(error),
        rpc_url: rpc_url.to_string(),
        explorer_url,
        selected_account: None,
        detail_scroll: 0,
    }
}

async fn fetch_block_transactions_with_retry(
    client: &reqwest::Client,
    rpc_url: &str,
    slot: u64,
    exclude_vote_transactions: bool,
) -> Result<(BlockFetchOutcome, Vec<TransactionEntry>), String> {
    const MAX_ATTEMPTS: usize = 4;

    for attempt in 0..MAX_ATTEMPTS {
        match fetch_block_transactions(
            client,
            rpc_url,
            slot,
            exclude_vote_transactions,
        )
        .await
        {
            Ok((BlockFetchOutcome::Entries, entries)) => {
                return Ok((BlockFetchOutcome::Entries, entries));
            }
            Ok((BlockFetchOutcome::SkipSlot, _)) => {
                return Ok((BlockFetchOutcome::SkipSlot, Vec::new()));
            }
            Ok((BlockFetchOutcome::RetryLater, _)) => {
                if attempt + 1 == MAX_ATTEMPTS {
                    return Ok((BlockFetchOutcome::RetryLater, Vec::new()));
                }
            }
            Err(err) => {
                if attempt + 1 == MAX_ATTEMPTS {
                    return Err(err);
                }
            }
        }

        tokio::time::sleep(Duration::from_millis(120)).await;
    }

    Ok((BlockFetchOutcome::RetryLater, Vec::new()))
}

async fn fetch_block_transactions(
    client: &reqwest::Client,
    rpc_url: &str,
    slot: u64,
    exclude_vote_transactions: bool,
) -> Result<(BlockFetchOutcome, Vec<TransactionEntry>), String> {
    let request_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getBlock",
        "params": [
            slot,
            {
                "encoding": "json",
                "transactionDetails": "full",
                "rewards": false,
                "commitment": "confirmed",
                "maxSupportedTransactionVersion": 255
            }
        ]
    });

    let response = client
        .post(rpc_url)
        .timeout(Duration::from_secs(5))
        .json(&request_body)
        .send()
        .await
        .map_err(|e| format!("HTTP error: {}", e))?;

    let rpc_response: RpcResponse<BlockResponse> = response
        .json()
        .await
        .map_err(|e| format!("Parse error: {}", e))?;

    if let Some(err) = rpc_response.error {
        if is_retryable_block_error(err.code) {
            return Ok((BlockFetchOutcome::RetryLater, Vec::new()));
        }
        if is_skippable_block_error(err.code) {
            return Ok((BlockFetchOutcome::SkipSlot, Vec::new()));
        }
        return Err(format!("{} (code {})", err.message, err.code));
    }

    let Some(block) = rpc_response.result else {
        return Ok((BlockFetchOutcome::RetryLater, Vec::new()));
    };

    let timestamp = Local::now();
    let entries = block
        .transactions
        .into_iter()
        .filter_map(|tx| {
            let signature = tx.transaction.signatures.into_iter().next()?;
            let should_exclude = exclude_vote_transactions
                && is_vote_transaction(
                    &tx.transaction.message,
                    tx.meta
                        .as_ref()
                        .and_then(|meta| meta.loaded_addresses.as_ref()),
                );
            if should_exclude {
                return None;
            }

            let mut accounts = tx.transaction.message.account_keys;
            let success =
                tx.meta.as_ref().map(|m| m.err.is_none()).unwrap_or(true);
            if let Some(loaded_addresses) =
                tx.meta.and_then(|m| m.loaded_addresses)
            {
                accounts.extend(loaded_addresses.writable);
                accounts.extend(loaded_addresses.readonly);
            }
            Some(TransactionEntry {
                signature,
                slot,
                success,
                timestamp,
                accounts,
            })
        })
        .collect();

    Ok((BlockFetchOutcome::Entries, entries))
}

fn build_transaction_accounts(
    message: TransactionMessage,
    loaded_addresses: Option<LoadedAddresses>,
) -> Vec<TransactionAccount> {
    let TransactionMessage {
        header,
        account_keys,
    } = message;
    let required_signatures = header.num_required_signatures;
    let signed_writable_cutoff =
        required_signatures.saturating_sub(header.num_readonly_signed_accounts);
    let unsigned_writable_cutoff = account_keys
        .len()
        .saturating_sub(header.num_readonly_unsigned_accounts);

    let mut accounts = Vec::with_capacity(
        account_keys.len()
            + loaded_addresses
                .as_ref()
                .map(|loaded| loaded.writable.len() + loaded.readonly.len())
                .unwrap_or(0),
    );

    for (index, pubkey) in account_keys.into_iter().enumerate() {
        let is_signer = index < required_signatures;
        let is_writable = if is_signer {
            index < signed_writable_cutoff
        } else {
            index < unsigned_writable_cutoff
        };
        accounts.push(TransactionAccount::new(pubkey, is_signer, is_writable));
    }

    if let Some(loaded_addresses) = loaded_addresses {
        accounts.extend(
            loaded_addresses
                .writable
                .into_iter()
                .map(|pubkey| TransactionAccount::new(pubkey, false, true)),
        );
        accounts.extend(
            loaded_addresses
                .readonly
                .into_iter()
                .map(|pubkey| TransactionAccount::new(pubkey, false, false)),
        );
    }

    accounts
}

fn is_vote_transaction(
    message: &BlockTransactionMessage,
    loaded_addresses: Option<&LoadedAddresses>,
) -> bool {
    message.instructions.iter().any(|instruction| {
        resolve_account_key(
            &message.account_keys,
            loaded_addresses,
            instruction.program_id_index,
        )
        .is_some_and(|program_id| program_id == VOTE_PROGRAM_ID)
    })
}

fn resolve_account_key<'a>(
    account_keys: &'a [String],
    loaded_addresses: Option<&'a LoadedAddresses>,
    index: usize,
) -> Option<&'a str> {
    if let Some(pubkey) = account_keys.get(index) {
        return Some(pubkey.as_str());
    }

    let loaded_addresses = loaded_addresses?;
    let loaded_index = index.checked_sub(account_keys.len())?;
    if let Some(pubkey) = loaded_addresses.writable.get(loaded_index) {
        return Some(pubkey.as_str());
    }

    let readonly_index =
        loaded_index.checked_sub(loaded_addresses.writable.len())?;
    loaded_addresses
        .readonly
        .get(readonly_index)
        .map(String::as_str)
}

fn is_retryable_block_error(code: i64) -> bool {
    matches!(
        code,
        JSON_RPC_SERVER_ERROR_BLOCK_NOT_AVAILABLE
            | JSON_RPC_SERVER_ERROR_BLOCK_STATUS_NOT_AVAILABLE_YET
    )
}

fn is_skippable_block_error(code: i64) -> bool {
    matches!(
        code,
        JSON_RPC_SERVER_ERROR_SLOT_SKIPPED
            | JSON_RPC_SERVER_ERROR_LONG_TERM_STORAGE_SLOT_SKIPPED
            | JSON_RPC_SERVER_ERROR_BLOCK_CLEANED_UP
            | JSON_RPC_SERVER_ERROR_TRANSACTION_HISTORY_NOT_AVAILABLE
    )
}

fn build_explorer_url(rpc_url: &str, signature: &str) -> String {
    let encoded_rpc = url_encode(rpc_url);
    format!(
        "https://explorer.solana.com/tx/{}?cluster=custom&customUrl={}",
        signature, encoded_rpc
    )
}

fn open_url_in_browser(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(url).spawn()?;
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open").arg(url).spawn()?;
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()?;
    }
    #[cfg(not(any(
        target_os = "macos",
        target_os = "linux",
        target_os = "windows"
    )))]
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Opening URLs is not supported on this platform",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use solana_rpc_client_api::custom_error::{
        JSON_RPC_SERVER_ERROR_BLOCK_NOT_AVAILABLE,
        JSON_RPC_SERVER_ERROR_BLOCK_STATUS_NOT_AVAILABLE_YET,
        JSON_RPC_SERVER_ERROR_LONG_TERM_STORAGE_SLOT_SKIPPED,
        JSON_RPC_SERVER_ERROR_SLOT_SKIPPED,
        JSON_RPC_SERVER_ERROR_TRANSACTION_HISTORY_NOT_AVAILABLE,
    };

    use super::{
        backfill_start_slot, build_transaction_accounts,
        build_transaction_detail_request, is_retryable_block_error,
        is_skippable_block_error, is_vote_transaction, live_feed_target_slot,
        BlockTransactionMessage, CompiledInstruction, LoadedAddresses,
        TransactionMessage, TransactionMessageHeader, VOTE_PROGRAM_ID,
    };

    #[test]
    fn build_transaction_accounts_marks_static_and_loaded_keys() {
        let accounts = build_transaction_accounts(
            TransactionMessage {
                header: TransactionMessageHeader {
                    num_required_signatures: 2,
                    num_readonly_signed_accounts: 1,
                    num_readonly_unsigned_accounts: 1,
                },
                account_keys: vec![
                    "signer-w".to_string(),
                    "signer-ro".to_string(),
                    "unsigned-w".to_string(),
                    "unsigned-ro".to_string(),
                ],
            },
            Some(LoadedAddresses {
                writable: vec!["lookup-w".to_string()],
                readonly: vec!["lookup-ro".to_string()],
            }),
        );

        assert_eq!(accounts.len(), 6);
        assert_eq!(accounts[0].pubkey, "signer-w");
        assert!(accounts[0].is_signer);
        assert!(accounts[0].is_writable);
        assert_eq!(accounts[1].pubkey, "signer-ro");
        assert!(accounts[1].is_signer);
        assert!(!accounts[1].is_writable);
        assert_eq!(accounts[2].pubkey, "unsigned-w");
        assert!(!accounts[2].is_signer);
        assert!(accounts[2].is_writable);
        assert_eq!(accounts[3].pubkey, "unsigned-ro");
        assert!(!accounts[3].is_signer);
        assert!(!accounts[3].is_writable);
        assert_eq!(accounts[4].pubkey, "lookup-w");
        assert!(!accounts[4].is_signer);
        assert!(accounts[4].is_writable);
        assert_eq!(accounts[5].pubkey, "lookup-ro");
        assert!(!accounts[5].is_signer);
        assert!(!accounts[5].is_writable);
    }

    #[test]
    fn vote_detection_checks_static_and_loaded_program_ids() {
        let static_vote = BlockTransactionMessage {
            account_keys: vec![
                "payer".to_string(),
                VOTE_PROGRAM_ID.to_string(),
            ],
            instructions: vec![CompiledInstruction {
                program_id_index: 1,
            }],
        };
        assert!(is_vote_transaction(&static_vote, None));

        let loaded_vote = BlockTransactionMessage {
            account_keys: vec!["payer".to_string()],
            instructions: vec![CompiledInstruction {
                program_id_index: 1,
            }],
        };
        assert!(is_vote_transaction(
            &loaded_vote,
            Some(&LoadedAddresses {
                writable: vec![VOTE_PROGRAM_ID.to_string()],
                readonly: vec![],
            }),
        ));

        let non_vote = BlockTransactionMessage {
            account_keys: vec![
                "payer".to_string(),
                "11111111111111111111111111111111".to_string(),
            ],
            instructions: vec![CompiledInstruction {
                program_id_index: 1,
            }],
        };
        assert!(!is_vote_transaction(&non_vote, None));
    }

    #[test]
    fn transaction_detail_request_uses_confirmed_commitment() {
        let request = build_transaction_detail_request("sig");

        assert_eq!(request["method"], "getTransaction");
        assert_eq!(request["params"][0], "sig");
        assert_eq!(request["params"][1]["encoding"], "json");
        assert_eq!(request["params"][1]["commitment"], "confirmed");
        assert_eq!(request["params"][1]["maxSupportedTransactionVersion"], 255);
    }

    #[test]
    fn live_feed_target_slot_tracks_latest_slot() {
        assert_eq!(live_feed_target_slot(0), 0);
        assert_eq!(live_feed_target_slot(1), 1);
        assert_eq!(live_feed_target_slot(42), 42);
    }

    #[test]
    fn backfill_start_slot_limits_recent_history_window() {
        assert_eq!(backfill_start_slot(12), 0);
        assert_eq!(backfill_start_slot(64), 0);
        assert_eq!(backfill_start_slot(96), 32);
    }

    #[test]
    fn block_errors_are_classified_by_retryability() {
        assert!(is_retryable_block_error(
            JSON_RPC_SERVER_ERROR_BLOCK_NOT_AVAILABLE
        ));
        assert!(is_retryable_block_error(
            JSON_RPC_SERVER_ERROR_BLOCK_STATUS_NOT_AVAILABLE_YET
        ));
        assert!(is_skippable_block_error(JSON_RPC_SERVER_ERROR_SLOT_SKIPPED));
        assert!(is_skippable_block_error(
            JSON_RPC_SERVER_ERROR_LONG_TERM_STORAGE_SLOT_SKIPPED
        ));
        assert!(is_skippable_block_error(
            JSON_RPC_SERVER_ERROR_TRANSACTION_HISTORY_NOT_AVAILABLE
        ));
        assert!(!is_skippable_block_error(
            JSON_RPC_SERVER_ERROR_BLOCK_NOT_AVAILABLE
        ));
    }
}
