//! Custom tracing layer for capturing logs to TUI

use tokio::sync::mpsc::UnboundedSender;
use tracing::{
    field::{Field, Visit},
    Event, Level, Subscriber,
};
use tracing_subscriber::{layer::Context, registry::LookupSpan, Layer};

use crate::state::LogEntry;

/// A tracing layer that captures log events and sends them to the TUI
pub struct TuiTracingLayer {
    tx: UnboundedSender<LogEntry>,
    min_level: Level,
}

impl TuiTracingLayer {
    /// Create a new TUI tracing layer
    pub fn new(tx: UnboundedSender<LogEntry>, min_level: Level) -> Self {
        Self { tx, min_level }
    }
}

impl<S> Layer<S> for TuiTracingLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();

        // Filter by level
        if metadata.level() < &self.min_level {
            return;
        }

        // Extract the message from the event
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);

        let entry = LogEntry::new(
            *metadata.level(),
            metadata.target().to_string(),
            visitor.message,
        );

        // Non-blocking send - if the channel is full or closed, we just drop the log
        let _ = self.tx.send(entry);
    }
}

/// Visitor to extract the message field from a tracing event
#[derive(Default)]
struct MessageVisitor {
    message: String,
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{:?}", value);
            self.message = self.message.trim_matches('"').to_string();
        } else if self.message.is_empty() {
            // Fallback: use first field as message
            self.message = format!("{:?}", value);
            self.message = self.message.trim_matches('"').to_string();
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" || self.message.is_empty() {
            self.message = value.to_string();
        }
    }
}
