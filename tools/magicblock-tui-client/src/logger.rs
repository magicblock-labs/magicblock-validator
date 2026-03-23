use std::fmt::Write;

use tokio::sync::mpsc::{
    unbounded_channel, UnboundedReceiver, UnboundedSender,
};
use tracing::{field::Visit, Event, Subscriber};
use tracing_subscriber::{
    layer::Context, prelude::*, registry::Registry, EnvFilter, Layer,
};

use crate::state::LogEntry;

struct TuiLogLayer {
    tx: UnboundedSender<LogEntry>,
}

#[derive(Default)]
struct TuiLogVisitor {
    message: Option<String>,
    fields: Vec<String>,
}

impl TuiLogVisitor {
    fn finish(self) -> String {
        match (self.message, self.fields.is_empty()) {
            (Some(message), true) => message,
            (Some(message), false) => {
                format!("{message} {}", self.fields.join(" "))
            }
            (None, false) => self.fields.join(" "),
            (None, true) => String::new(),
        }
    }
}

impl Visit for TuiLogVisitor {
    fn record_debug(
        &mut self,
        field: &tracing::field::Field,
        value: &dyn std::fmt::Debug,
    ) {
        if field.name() == "message" {
            self.message =
                Some(format!("{value:?}").trim_matches('"').to_string());
            return;
        }

        let mut rendered = String::new();
        let _ = write!(&mut rendered, "{}={value:?}", field.name());
        self.fields.push(rendered);
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = Some(value.to_string());
        } else {
            self.fields.push(format!("{}={value}", field.name()));
        }
    }
}

impl<S> Layer<S> for TuiLogLayer
where
    S: Subscriber,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();
        let mut visitor = TuiLogVisitor::default();
        event.record(&mut visitor);

        let _ = self.tx.send(LogEntry::new(
            *metadata.level(),
            metadata.target().to_string(),
            visitor.finish(),
        ));
    }
}

pub fn init_embedded_logger() -> UnboundedReceiver<LogEntry> {
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));
    let (tx, rx) = unbounded_channel();

    Registry::default()
        .with(env_filter)
        .with(TuiLogLayer { tx })
        .init();

    rx
}
