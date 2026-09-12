//! Telemetry: the engine's log path, matching the tenders' house pattern.
//!
//! The tenders ship their logs through their own lib (otel_logging) via
//! OTLP to the observability collector, which forwards to Loki. This
//! module gives the Rust engine the same channel: every `log` record is
//! exported OTLP/HTTP to the collector AND written to stdout - the engine
//! never logs less because telemetry is down.
//!
//! Contract (identical to the tenders' lib):
//!   OTEL_EXPORTER_OTLP_ENDPOINT - collector base URL; default is the
//!     same collector DNS the tenders default to
//!   OTEL_SERVICE_NAME           - logical name; default "godon-causal"
//!
//! Export is batched (background thread, never blocks request handling).
//! Init is best-effort: a broken endpoint degrades to stdout-only with
//! one loud line on stderr - it must never take the engine down.

use log::{LevelFilter, Log, Metadata, Record};
use opentelemetry_otlp::{LogExporter, Protocol, WithExportConfig};
use opentelemetry_sdk::logs::{BatchLogProcessor, SdkLogger, SdkLoggerProvider};
use opentelemetry_sdk::resource::Resource;

/// Same collector the tenders' otel_logging defaults to.
pub const DEFAULT_OTLP_ENDPOINT: &str =
    "http://godon-observability-opentelemetry-collector.godon-observability.svc.cluster.local:4318";

/// This engine's logical name in Loki.
pub const DEFAULT_SERVICE_NAME: &str = "godon-causal";

type OtBridge = opentelemetry_appender_log::OpenTelemetryLogBridge<SdkLoggerProvider, SdkLogger>;

/// One facade, two sinks: stdout (env_logger, unchanged behavior) and
/// the OTLP bridge into the collector. Both see every record.
struct DualLogger {
    stdout: env_logger::Logger,
    otel: OtBridge,
}

impl Log for DualLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        self.stdout.enabled(metadata) || self.otel.enabled(metadata)
    }

    fn log(&self, record: &Record) {
        if self.stdout.enabled(record.metadata()) {
            self.stdout.log(record);
        }
        if self.otel.enabled(record.metadata()) {
            self.otel.log(record);
        }
    }

    fn flush(&self) {
        self.stdout.flush();
    }
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn build_otel_bridge() -> Result<OtBridge, String> {
    let endpoint = env_or("OTEL_EXPORTER_OTLP_ENDPOINT", DEFAULT_OTLP_ENDPOINT);
    let service_name = env_or("OTEL_SERVICE_NAME", DEFAULT_SERVICE_NAME);

    let exporter = LogExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint(format!("{endpoint}/v1/logs"))
        .build()
        .map_err(|e| format!("otlp log exporter build failed: {e}"))?;

    let provider = SdkLoggerProvider::builder()
        .with_resource(Resource::builder().with_service_name(service_name).build())
        .with_log_processor(BatchLogProcessor::builder(exporter).build())
        .build();

    Ok(opentelemetry_appender_log::OpenTelemetryLogBridge::new(
        &provider,
    ))
}

/// Install the dual logger. Called once from main before any routing
/// starts. The level comes from RUST_LOG as before (default Info) and
/// governs both sinks.
pub fn init() {
    let stdout = env_logger::Builder::from_default_env().build();
    let level: LevelFilter = stdout.filter();

    let logger: Box<dyn Log> = match build_otel_bridge() {
        Ok(otel) => {
            eprintln!(
                "otel logging: exporting to {} as {} (stdout stays on)",
                env_or("OTEL_EXPORTER_OTLP_ENDPOINT", DEFAULT_OTLP_ENDPOINT),
                env_or("OTEL_SERVICE_NAME", DEFAULT_SERVICE_NAME)
            );
            Box::new(DualLogger { stdout, otel })
        }
        Err(e) => {
            eprintln!("otel init failed - stdout logging only: {e}");
            Box::new(stdout)
        }
    };

    let _ = log::set_boxed_logger(logger);
    log::set_max_level(level);
}
