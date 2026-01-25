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
use flume::Receiver as MpmcReceiver;
use magicblock_core::link::{blocks::BlockUpdateRx, transactions::TransactionStatus};
use ratatui::{backend::CrosstermBackend, Terminal};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::broadcast::error::TryRecvError;
use tokio_util::sync::CancellationToken;
use tracing::Level;
use tracing_log::LogTracer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use crate::{
    events::{handle_event, poll_event},
    logger::TuiTracingLayer,
    state::{LogEntry, TransactionEntry, TuiConfig, TuiState},
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
    tx_status_rx: MpmcReceiver<TransactionStatus>,
    cancel: CancellationToken,
) -> io::Result<()> {
    // Set up log capture channel
    let (log_tx, log_rx) = tokio::sync::mpsc::unbounded_channel();

    // Initialize the tracing subscriber with our custom layer
    let _ = LogTracer::init();
    let tui_layer = TuiTracingLayer::new(log_tx, Level::INFO);
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

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
    tx_status_rx: MpmcReceiver<TransactionStatus>,
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

            handle_event(state, event, visible_height);

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
        while let Ok(tx_status) = tx_status_rx.try_recv() {
            let entry = TransactionEntry {
                signature: tx_status.txn.signature().to_string(),
                slot: tx_status.slot,
                success: tx_status.meta.status.is_ok(),
                timestamp: Utc::now(),
            };
            state.push_transaction(entry);
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
