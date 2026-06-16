//! Runtime tracing and OpenTelemetry setup.

use std::env;

use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{trace::SdkTracerProvider, Resource};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{fmt, EnvFilter};

/// Keeps the OpenTelemetry tracer provider alive until process shutdown.
///
/// Dropping the guard flushes and shuts down the provider. Binaries should keep
/// this value in scope for the lifetime of the process.
#[derive(Debug)]
pub struct TelemetryGuard {
    tracer_provider: Option<SdkTracerProvider>,
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        if let Some(provider) = &self.tracer_provider {
            if let Err(err) = provider.shutdown() {
                eprintln!("failed to shut down OpenTelemetry tracer provider: {err}");
            }
        }
    }
}

/// Initialize process-wide tracing.
///
/// OpenTelemetry export is enabled when either `SHUTTLE_OTEL` is truthy or an
/// OTLP endpoint is configured via `OTEL_EXPORTER_OTLP_ENDPOINT` or
/// `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`. Human-readable logs are always emitted
/// to stderr so CLI JSON output on stdout remains machine-readable.
pub fn init(service_name: &'static str) -> TelemetryGuard {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    let fmt_layer = fmt::layer().with_writer(std::io::stderr);

    let tracer_provider = if otel_enabled() {
        match build_tracer_provider(service_name) {
            Ok(provider) => Some(provider),
            Err(err) => {
                eprintln!("failed to initialize OpenTelemetry exporter: {err}");
                None
            }
        }
    } else {
        None
    };

    if let Some(provider) = tracer_provider.clone() {
        let tracer = provider.tracer(service_name);
        tracing_subscriber::registry()
            .with(filter)
            .with(fmt_layer)
            .with(tracing_opentelemetry::layer().with_tracer(tracer))
            .init();
    } else {
        tracing_subscriber::registry()
            .with(filter)
            .with(fmt_layer)
            .init();
    }

    TelemetryGuard { tracer_provider }
}

fn build_tracer_provider(service_name: &'static str) -> Result<SdkTracerProvider, String> {
    let endpoint = env::var("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT")
        .or_else(|_| env::var("OTEL_EXPORTER_OTLP_ENDPOINT"))
        .unwrap_or_else(|_| "http://localhost:4317".to_owned());
    let service_name = env::var("OTEL_SERVICE_NAME").unwrap_or_else(|_| service_name.to_owned());
    let resource = Resource::builder().with_service_name(service_name).build();
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_endpoint(endpoint)
        .build()
        .map_err(|err| err.to_string())?;

    Ok(SdkTracerProvider::builder()
        .with_resource(resource)
        .with_simple_exporter(exporter)
        .build())
}

fn otel_enabled() -> bool {
    env_truthy("SHUTTLE_OTEL")
        || env_truthy("SHUTTLE_OTEL_ENABLED")
        || env::var_os("OTEL_EXPORTER_OTLP_ENDPOINT").is_some()
        || env::var_os("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT").is_some()
}

fn env_truthy(name: &str) -> bool {
    env::var(name)
        .map(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_truthy_accepts_common_truthy_values() {
        env::set_var("SHUTTLE_TEST_TRUTHY", "yes");
        assert!(env_truthy("SHUTTLE_TEST_TRUTHY"));
        env::set_var("SHUTTLE_TEST_TRUTHY", "false");
        assert!(!env_truthy("SHUTTLE_TEST_TRUTHY"));
        env::remove_var("SHUTTLE_TEST_TRUTHY");
    }
}
