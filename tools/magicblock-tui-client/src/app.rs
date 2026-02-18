use std::{
    io::{self, Stdout},
    panic,
    time::Duration,
};

use chrono::Utc;
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
        LogEntry, TransactionDetail, TransactionEntry, TuiConfig, TuiState,
    },
    ui,
    utils::url_encode,
};

type Term = Terminal<CrosstermBackend<Stdout>>;

#[derive(Debug)]
enum AppEvent {
    Slot(u64),
    Transaction(TransactionEntry),
    TransactionDetail(TransactionDetail),
    Log(LogEntry),
}

pub async fn run_tui(config: TuiConfig) -> io::Result<()> {
    let cancel = CancellationToken::new();
    setup_panic_hook();
    let mut terminal = init_terminal()?;
    let mut state = TuiState::new(config.clone());
    let client = reqwest::Client::new();
    if let Ok(epoch_info) = get_epoch_info(&client, &config.rpc_url).await {
        if epoch_info.slots_in_epoch > 0 {
            state.slots_per_epoch = epoch_info.slots_in_epoch;
        }
    }

    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let (slot_tx, slot_rx) = mpsc::unbounded_channel();
    spawn_slot_subscription(
        config.ws_url.clone(),
        event_tx.clone(),
        slot_tx,
        cancel.clone(),
    );
    spawn_block_transaction_feed(
        config.rpc_url.clone(),
        slot_rx,
        event_tx.clone(),
        cancel.clone(),
    );
    spawn_logs_subscription(
        config.ws_url.clone(),
        event_tx.clone(),
        cancel.clone(),
    );

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

        let visible_height = terminal
            .size()
            .map(|rect| rect.height.saturating_sub(9) as usize)
            .unwrap_or(0);

        if let Some(event) = poll_event(poll_timeout) {
            let is_resize = matches!(event, Event::Resize(_, _));

            let action = handle_event(state, event, visible_height);
            match action {
                EventAction::FetchTransaction(sig) => {
                    let rpc_url = state.rpc_url.clone();
                    let client = rpc_client.clone();
                    let event_tx = event_tx.clone();

                    tokio::spawn(async move {
                        let detail = match fetch_transaction_detail(
                            &client, &rpc_url, &sig,
                        )
                        .await
                        {
                            Ok(detail) => detail,
                            Err(e) => build_failed_tx_detail(
                                &rpc_url,
                                sig,
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
                AppEvent::Transaction(tx) => state.push_transaction(tx),
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
    event_tx: UnboundedSender<AppEvent>,
    slot_tx: UnboundedSender<u64>,
    cancel: CancellationToken,
) {
    tokio::spawn(async move {
        while !cancel.is_cancelled() {
            match PubsubClient::new(&ws_url).await {
                Ok(client) => {
                    let _ = event_tx.send(AppEvent::Log(LogEntry::new(
                        Level::INFO,
                        "slot_subscribe".to_string(),
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
                                                let _ = event_tx.send(AppEvent::Slot(slot));
                                                let _ = slot_tx.send(slot);
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
                                    "slot_subscribe".to_string(),
                                    format!("Subscription failed: {}", err),
                                )));
                        }
                    }
                }
                Err(err) => {
                    let _ = event_tx.send(AppEvent::Log(LogEntry::new(
                        Level::ERROR,
                        "slot_subscribe".to_string(),
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
    mut slot_rx: UnboundedReceiver<u64>,
    event_tx: UnboundedSender<AppEvent>,
    cancel: CancellationToken,
) {
    tokio::spawn(async move {
        let client = reqwest::Client::new();
        let mut latest_target_slot: Option<u64> = None;
        let mut next_slot_to_fetch: Option<u64> = None;

        let _ = event_tx.send(AppEvent::Log(LogEntry::new(
            Level::INFO,
            "tx_feed".to_string(),
            format!("Using getBlock transaction feed on {}", rpc_url),
        )));

        while !cancel.is_cancelled() {
            tokio::select! {
                _ = cancel.cancelled() => break,
                maybe_slot = slot_rx.recv() => {
                    let slot = match maybe_slot {
                        Some(slot) => slot,
                        None => break,
                    };

                    // Stay behind the latest slot to reduce transient `null` blocks.
                    let target_slot = slot.saturating_sub(2);
                    latest_target_slot = Some(match latest_target_slot {
                        Some(current) => current.max(target_slot),
                        None => target_slot,
                    });

                    if next_slot_to_fetch.is_none() {
                        next_slot_to_fetch = Some(target_slot);
                    }

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
                        )
                        .await
                        {
                            Ok(Some(entries)) => {
                                for entry in entries {
                                    let _ = event_tx.send(AppEvent::Transaction(entry));
                                }
                                true
                            }
                            Ok(None) => true,
                            Err(err) => {
                                let _ = event_tx.send(AppEvent::Log(LogEntry::new(
                                    Level::WARN,
                                    "tx_feed".to_string(),
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
}

#[derive(Debug, Deserialize)]
struct BlockTransactionMeta {
    err: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct TransactionMeta {
    fee: u64,
    err: Option<serde_json::Value>,
    #[serde(rename = "logMessages")]
    log_messages: Option<Vec<String>>,
    #[serde(rename = "computeUnitsConsumed")]
    compute_units_consumed: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct TransactionData {
    message: TransactionMessage,
}

#[derive(Debug, Deserialize)]
struct TransactionMessage {
    #[serde(rename = "accountKeys")]
    account_keys: Vec<String>,
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

async fn fetch_transaction_detail(
    client: &reqwest::Client,
    rpc_url: &str,
    signature: &str,
) -> Result<TransactionDetail, String> {
    let request_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getTransaction",
        "params": [
            signature,
            {
                "encoding": "json",
                "maxSupportedTransactionVersion": 255
            }
        ]
    });

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

    let meta = tx.meta.as_ref();
    let success = meta.map(|m| m.err.is_none()).unwrap_or(true);
    let error = meta.and_then(|m| m.err.as_ref()).map(|e| format!("{}", e));

    Ok(TransactionDetail {
        signature: signature.to_string(),
        slot: tx.slot,
        success,
        fee: meta.map(|m| m.fee).unwrap_or(0),
        compute_units: meta.and_then(|m| m.compute_units_consumed),
        logs: meta
            .and_then(|m| m.log_messages.clone())
            .unwrap_or_default(),
        accounts: tx.transaction.message.account_keys,
        error,
        explorer_url: build_explorer_url(rpc_url, signature),
        explorer_selected: false,
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
        explorer_url,
        explorer_selected: false,
    }
}

async fn fetch_block_transactions_with_retry(
    client: &reqwest::Client,
    rpc_url: &str,
    slot: u64,
) -> Result<Option<Vec<TransactionEntry>>, String> {
    const MAX_ATTEMPTS: usize = 4;

    for attempt in 0..MAX_ATTEMPTS {
        match fetch_block_transactions(client, rpc_url, slot).await {
            Ok(Some(entries)) => return Ok(Some(entries)),
            Ok(None) => {
                if attempt + 1 == MAX_ATTEMPTS {
                    return Ok(None);
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

    Ok(None)
}

async fn fetch_block_transactions(
    client: &reqwest::Client,
    rpc_url: &str,
    slot: u64,
) -> Result<Option<Vec<TransactionEntry>>, String> {
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
        if is_non_fatal_block_error(err.code) {
            return Ok(None);
        }
        return Err(format!("{} (code {})", err.message, err.code));
    }

    let Some(block) = rpc_response.result else {
        return Ok(None);
    };

    let timestamp = Utc::now();
    let entries = block
        .transactions
        .into_iter()
        .filter_map(|tx| {
            let signature = tx.transaction.signatures.into_iter().next()?;
            let success = tx.meta.map(|m| m.err.is_none()).unwrap_or(true);
            Some(TransactionEntry {
                signature,
                slot,
                success,
                timestamp,
            })
        })
        .collect();

    Ok(Some(entries))
}

fn is_non_fatal_block_error(code: i64) -> bool {
    matches!(
        code,
        JSON_RPC_SERVER_ERROR_BLOCK_NOT_AVAILABLE
            | JSON_RPC_SERVER_ERROR_BLOCK_STATUS_NOT_AVAILABLE_YET
            | JSON_RPC_SERVER_ERROR_SLOT_SKIPPED
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
