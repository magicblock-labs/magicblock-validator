use tracing_log::LogTracer;
use tracing_subscriber::{fmt, EnvFilter};

mod consolidate;
pub use consolidate::log_trace_warn;

/// Initialize the tracing subscriber for the main validator.
///
/// This must be called once at application startup.
/// It respects the `RUST_LOG` environment variable for filtering.
pub fn init() {
    init_with_config(LoggingConfig::default())
}

/// Initialize tracing with custom configuration.
pub fn init_with_config(config: LoggingConfig) {
    // Capture log records from dependencies using the `log` crate
    LogTracer::init().expect("Failed to set LogTracer");

    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));

    match config.style {
        LogStyle::Default => {
            let subscriber = fmt::Subscriber::builder()
                .with_env_filter(env_filter)
                .with_timer(fmt::time::UtcTime::rfc_3339())
                .finish();
            tracing::subscriber::set_global_default(subscriber)
                .expect("Failed to set subscriber");
        }
        LogStyle::Ephem => {
            let subscriber = fmt::Subscriber::builder()
                .with_env_filter(env_filter)
                .fmt_fields(EphemFieldFormatter)
                .event_format(EphemEventFormatter)
                .finish();
            tracing::subscriber::set_global_default(subscriber)
                .expect("Failed to set subscriber");
        }
        LogStyle::Devnet => {
            let subscriber = fmt::Subscriber::builder()
                .with_env_filter(env_filter)
                .fmt_fields(DevnetFieldFormatter)
                .event_format(DevnetEventFormatter)
                .finish();
            tracing::subscriber::set_global_default(subscriber)
                .expect("Failed to set subscriber");
        }
    }
}

/// Initialize tracing for tests.
///
/// Uses `try_init()` to allow multiple test threads to call this safely.
/// Captures test output correctly with `TestWriter`.
pub fn init_for_tests() {
    let _ = LogTracer::init();

    let _ = fmt::Subscriber::builder()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_test_writer()
        .try_init();
}

#[derive(Default)]
pub struct LoggingConfig {
    pub style: LogStyle,
}

#[derive(Default)]
pub enum LogStyle {
    #[default]
    Default,
    Ephem,
    Devnet,
}

impl LogStyle {
    pub fn from_env() -> Self {
        match std::env::var("RUST_LOG_STYLE").as_deref() {
            Ok("EPHEM") => LogStyle::Ephem,
            Ok("DEVNET") => LogStyle::Devnet,
            _ => LogStyle::Default,
        }
    }
}

// -----------------
// Custom formatters for EPHEM and DEVNET styles
// -----------------
use tracing::{Event, Subscriber};
use tracing_subscriber::{
    fmt::{
        format::{self, FormatEvent, FormatFields, Writer},
        time::FormatTime,
        FmtContext,
    },
    registry::LookupSpan,
};

/// Field formatter for EPHEM style - writes fields as space-separated values
pub struct EphemFieldFormatter;

impl<'writer> FormatFields<'writer> for EphemFieldFormatter {
    fn format_fields<
        R: tracing_subscriber::prelude::__tracing_subscriber_field_RecordFields,
    >(
        &self,
        mut writer: Writer<'writer>,
        fields: R,
    ) -> std::fmt::Result {
        let mut visitor = SpaceSeparatedVisitor::new(&mut writer);
        fields.record(&mut visitor);
        Ok(())
    }
}

/// Field formatter for DEVNET style - writes fields as space-separated values
pub struct DevnetFieldFormatter;

impl<'writer> FormatFields<'writer> for DevnetFieldFormatter {
    fn format_fields<
        R: tracing_subscriber::prelude::__tracing_subscriber_field_RecordFields,
    >(
        &self,
        mut writer: Writer<'writer>,
        fields: R,
    ) -> std::fmt::Result {
        let mut visitor = SpaceSeparatedVisitor::new(&mut writer);
        fields.record(&mut visitor);
        Ok(())
    }
}

/// Visitor that formats fields as space-separated values
struct SpaceSeparatedVisitor<'a, 'writer> {
    writer: &'a mut Writer<'writer>,
    first: bool,
}

impl<'a, 'writer> SpaceSeparatedVisitor<'a, 'writer> {
    fn new(writer: &'a mut Writer<'writer>) -> Self {
        Self {
            writer,
            first: true,
        }
    }
}

impl tracing::field::Visit for SpaceSeparatedVisitor<'_, '_> {
    fn record_debug(
        &mut self,
        field: &tracing::field::Field,
        value: &dyn std::fmt::Debug,
    ) {
        if field.name() == "message" {
            let _ = write!(self.writer, "{:?}", value);
        } else {
            if !self.first {
                let _ = write!(self.writer, " ");
            }
            let _ = write!(self.writer, "{}={:?}", field.name(), value);
            self.first = false;
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            let _ = write!(self.writer, "{}", value);
        } else {
            if !self.first {
                let _ = write!(self.writer, " ");
            }
            let _ = write!(self.writer, "{}={}", field.name(), value);
            self.first = false;
        }
    }
}

/// Event formatter for EPHEM style
pub struct EphemEventFormatter;

impl<S, N> FormatEvent<S, N> for EphemEventFormatter
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: format::Writer<'_>,
        event: &Event<'_>,
    ) -> std::fmt::Result {
        let metadata = event.metadata();
        let level = metadata.level();
        let target = metadata.target();

        // Format timestamp
        let timer = fmt::time::UtcTime::rfc_3339();
        write!(writer, "EPHEM [{}] ", level)?;
        timer.format_time(&mut writer)?;
        write!(writer, ": {} ", target)?;

        // Format fields (including message)
        ctx.field_format().format_fields(writer.by_ref(), event)?;
        writeln!(writer)
    }
}

/// Event formatter for DEVNET style
pub struct DevnetEventFormatter;

impl<S, N> FormatEvent<S, N> for DevnetEventFormatter
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: format::Writer<'_>,
        event: &Event<'_>,
    ) -> std::fmt::Result {
        let metadata = event.metadata();
        let level = metadata.level();
        let target = metadata.target();

        // Format timestamp
        let timer = fmt::time::UtcTime::rfc_3339();
        write!(writer, "DEVNET [{}] ", level)?;
        timer.format_time(&mut writer)?;
        write!(writer, ": {} ", target)?;

        // Format fields (including message)
        ctx.field_format().format_fields(writer.by_ref(), event)?;
        writeln!(writer)
    }
}
