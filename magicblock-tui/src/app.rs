//! Main TUI application and event loop

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
use magicblock_core::link::{
    blocks::BlockUpdateRx, transactions::TransactionStatusRx,
};
use ratatui::{backend::CrosstermBackend, Terminal};
use serde::Deserialize;
use tokio::sync::{broadcast::error::TryRecvError, mpsc::UnboundedReceiver};
use tokio_util::sync::CancellationToken;
use tracing::Level;
use tracing_log::LogTracer;
use tracing_subscriber::{
    layer::SubscriberExt, util::SubscriberInitExt, EnvFilter,
};

use crate::{
    events::{handle_event, poll_event, EventAction},
    logger::TuiTracingLayer,
    state::{
        LogEntry, TransactionDetail, TransactionEntry, TuiConfig, TuiState,
    },
    ui,
};

/// Terminal type alias
type Term = Terminal<CrosstermBackend<Stdout>>;

/// Run the TUI application
///
/// This function takes over the terminal, displays the TUI, and blocks until
/// the user quits or the cancellation token is triggered.
pub async fn run_tui(
    config: TuiConfig,
    block_rx: BlockUpdateRx,
    tx_status_rx: TransactionStatusRx,
    cancel: CancellationToken,
) -> io::Result<()> {
    // Set up log capture channel
    let (log_tx, log_rx) = tokio::sync::mpsc::unbounded_channel();

    // Initialize the tracing subscriber with our custom layer
    let _ = LogTracer::init();
    let tui_layer = TuiTracingLayer::new(log_tx, Level::INFO);
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(env_filter)
        .with(tui_layer)
        .try_init()
        .ok();

    // Set up panic hook to restore terminal
    setup_panic_hook();

    // Initialize terminal
    let mut terminal = init_terminal()?;

    // Create TUI state
    let mut state = TuiState::new(config);

    // Run the event loop
    let result = run_event_loop(
        &mut terminal,
        &mut state,
        block_rx,
        tx_status_rx,
        log_rx,
        cancel,
    )
    .await;

    // Restore terminal
    restore_terminal(&mut terminal)?;

    result
}

/// Set up a panic hook that restores the terminal on panic
fn setup_panic_hook() {
    let original_hook = panic::take_hook();
    panic::set_hook(Box::new(move |panic_info| {
        // Restore terminal
        let _ = terminal::disable_raw_mode();
        let _ = io::stdout().execute(LeaveAlternateScreen);
        let _ = io::stdout().execute(cursor::Show);
        original_hook(panic_info);
    }));
}

/// Initialize the terminal for TUI rendering
fn init_terminal() -> io::Result<Term> {
    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    stdout.execute(cursor::Hide)?;
    Terminal::new(CrosstermBackend::new(stdout))
}

/// Restore the terminal to its original state
fn restore_terminal(terminal: &mut Term) -> io::Result<()> {
    terminal::disable_raw_mode()?;
    terminal.backend_mut().execute(LeaveAlternateScreen)?;
    terminal.backend_mut().execute(cursor::Show)?;
    Ok(())
}

/// Main event loop
async fn run_event_loop(
    terminal: &mut Term,
    state: &mut TuiState,
    mut block_rx: BlockUpdateRx,
    mut tx_status_rx: TransactionStatusRx,
    mut log_rx: UnboundedReceiver<LogEntry>,
    cancel: CancellationToken,
) -> io::Result<()> {
    // Use zero timeout for non-blocking poll - we'll use tokio for timing
    let poll_timeout = Duration::ZERO;
    let tick_rate = Duration::from_millis(50);
    let mut last_tick = std::time::Instant::now();

    // Initial draw
    terminal.draw(|f| ui::render(f, state))?;

    loop {
        // Check for cancellation
        if cancel.is_cancelled() {
            state.should_quit = true;
        }

        if state.should_quit {
            // Signal cancellation to the rest of the system
            cancel.cancel();
            break;
        }

        // Get visible content height for scroll calculations
        let visible_height = terminal
            .size()
            .map(|rect| rect.height.saturating_sub(9) as usize)
            .unwrap_or(0);

        // Poll for keyboard events (non-blocking with zero timeout)
        if let Some(event) = poll_event(poll_timeout) {
            // Check for resize before handling (event will be moved)
            let is_resize = matches!(event, Event::Resize(_, _));

            let action = handle_event(state, event, visible_height);

            // Handle any resulting action
            match action {
                EventAction::FetchTransaction(sig) => {
                    // Fetch transaction details from RPC
                    let rpc_url = state.rpc_url.clone();
                    match fetch_transaction_detail(&rpc_url, &sig).await {
                        Ok(detail) => {
                            state.show_tx_detail(detail);
                        }
                        Err(e) => {
                            // Show error in detail view
                            let explorer_url =
                                build_explorer_url(&state.rpc_url, &sig);
                            state.show_tx_detail(TransactionDetail {
                                signature: sig,
                                slot: 0,
                                success: false,
                                fee: 0,
                                compute_units: None,
                                logs: vec![],
                                accounts: vec![],
                                error: Some(format!("Failed to fetch: {}", e)),
                                explorer_url,
                                explorer_selected: false,
                            });
                        }
                    }
                }
                EventAction::OpenUrl(url) => {
                    // Open URL in default browser
                    let _ = open_url_in_browser(&url);
                }
                EventAction::None => {}
            }

            // If resize, redraw immediately
            if is_resize {
                terminal.draw(|f| ui::render(f, state))?;
                continue;
            }
        }

        // Process incoming block updates (non-blocking)
        loop {
            match block_rx.try_recv() {
                Ok(block) => {
                    state.update_slot(block.meta.slot);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Lagged(_)) => continue,
                Err(TryRecvError::Closed) => break,
            }
        }

        // Process incoming transaction status (non-blocking)
        loop {
            match tx_status_rx.try_recv() {
                Ok(tx_status) => {
                    let entry = TransactionEntry {
                        signature: tx_status.txn.signature().to_string(),
                        slot: tx_status.slot,
                        success: tx_status.meta.status.is_ok(),
                        timestamp: Utc::now(),
                    };
                    state.push_transaction(entry);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Lagged(_)) => continue,
                Err(TryRecvError::Closed) => break,
            }
        }

        // Process incoming log entries (non-blocking)
        while let Ok(log) = log_rx.try_recv() {
            state.push_log(log);
        }

        // Draw UI at tick rate
        if last_tick.elapsed() >= tick_rate {
            terminal.draw(|f| ui::render(f, state))?;
            last_tick = std::time::Instant::now();
        }

        // Yield to tokio runtime - this is critical for async tasks to make progress
        tokio::time::sleep(Duration::from_millis(1)).await;
    }

    Ok(())
}

/// RPC response structures for parsing getTransaction response
#[derive(Debug, Deserialize)]
struct RpcResponse<T> {
    result: Option<T>,
    #[allow(dead_code)]
    error: Option<RpcError>,
}

#[derive(Debug, Deserialize)]
struct RpcError {
    message: String,
}

#[derive(Debug, Deserialize)]
struct TransactionResponse {
    slot: u64,
    meta: Option<TransactionMeta>,
    transaction: TransactionData,
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

/// Fetch transaction details from RPC
async fn fetch_transaction_detail(
    rpc_url: &str,
    signature: &str,
) -> Result<TransactionDetail, String> {
    let client = reqwest::Client::new();

    let request_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getTransaction",
        "params": [
            signature,
            {
                "encoding": "json",
                "maxSupportedTransactionVersion": 0
            }
        ]
    });

    let response = client
        .post(rpc_url)
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

/// Build the Solana Explorer URL for a transaction
fn build_explorer_url(rpc_url: &str, signature: &str) -> String {
    let encoded_rpc = url_encode(rpc_url);
    format!(
        "https://explorer.solana.com/tx/{}?cluster=custom&customUrl={}",
        signature, encoded_rpc
    )
}

/// Simple URL encoding
fn url_encode(s: &str) -> String {
    let mut encoded = String::with_capacity(s.len() * 3);
    for c in s.chars() {
        match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => {
                encoded.push(c);
            }
            _ => {
                for byte in c.to_string().as_bytes() {
                    encoded.push_str(&format!("%{:02X}", byte));
                }
            }
        }
    }
    encoded
}

/// Open a URL in the default browser
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
            .args(["/C", "start", url])
            .spawn()?;
    }
    Ok(())
}
