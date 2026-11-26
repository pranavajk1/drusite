// SPDX-License-Identifier: AGPL-3.0-or-later

//! Logs, traces and metrics.
//!
//! Logs always go to stderr (`RUST_LOG` sets the level). With OTLP on, traces, metrics and logs
//! also go to any OTLP/HTTP backend, configured by the standard `OTEL_*` variables
//! (`OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_HEADERS`, …). Incoming W3C
//! `traceparent` headers are honoured, so a trace can start in a client or a proxy.

use std::time::Instant;

use axum::extract::{MatchedPath, Request};
use axum::middleware::Next;
use axum::response::Response;
use opentelemetry::global;
use opentelemetry::metrics::Histogram;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry::{KeyValue, propagation::Extractor};
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::logs::SdkLoggerProvider;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

use crate::config::{LogFormat, TelemetryConfig};

/// Flushes and shuts down the exporters when dropped, at the end of `main`.
#[derive(Debug, Default)]
pub struct Telemetry {
    tracer: Option<SdkTracerProvider>,
    meter: Option<SdkMeterProvider>,
    logger: Option<SdkLoggerProvider>,
}

impl Drop for Telemetry {
    fn drop(&mut self) {
        if let Some(t) = self.tracer.take() {
            let _ = t.shutdown();
        }
        if let Some(m) = self.meter.take() {
            let _ = m.shutdown();
        }
        if let Some(l) = self.logger.take() {
            let _ = l.shutdown();
        }
    }
}

fn otlp_enabled(config: &TelemetryConfig) -> bool {
    config.otlp.unwrap_or_else(|| {
        [
            "OTEL_EXPORTER_OTLP_ENDPOINT",
            "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
        ]
        .iter()
        .any(|v| std::env::var_os(v).is_some_and(|s| !s.is_empty()))
    })
}

/// Sets up logging, and OTLP export if it's on.
pub fn init(config: &TelemetryConfig) -> anyhow::Result<Telemetry> {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,sqlx=warn,tower_http=info"));
    let fmt = match config.log_format {
        LogFormat::Json => tracing_subscriber::fmt::layer()
            .json()
            .with_writer(std::io::stderr)
            .boxed(),
        LogFormat::Pretty => tracing_subscriber::fmt::layer()
            .with_writer(std::io::stderr)
            .boxed(),
    };

    if !otlp_enabled(config) {
        tracing_subscriber::registry()
            .with(filter)
            .with(fmt)
            .try_init()?;
        return Ok(Telemetry::default());
    }

    global::set_text_map_propagator(TraceContextPropagator::new());
    let resource = Resource::builder()
        .with_service_name(config.service_name.clone())
        .with_attribute(KeyValue::new("service.version", env!("CARGO_PKG_VERSION")))
        .build();

    let tracer_provider = SdkTracerProvider::builder()
        .with_batch_exporter(
            opentelemetry_otlp::SpanExporter::builder()
                .with_http()
                .build()?,
        )
        .with_resource(resource.clone())
        .build();
    let meter_provider = SdkMeterProvider::builder()
        .with_periodic_exporter(
            opentelemetry_otlp::MetricExporter::builder()
                .with_http()
                .build()?,
        )
        .with_resource(resource.clone())
        .build();
    let logger_provider = SdkLoggerProvider::builder()
        .with_batch_exporter(
            opentelemetry_otlp::LogExporter::builder()
                .with_http()
                .build()?,
        )
        .with_resource(resource)
        .build();
    global::set_tracer_provider(tracer_provider.clone());
    global::set_meter_provider(meter_provider.clone());

    // The exporters log through `tracing` themselves; keep those out of the OTLP log stream so
    // a failing exporter can't feed itself.
    let otel_logs = OpenTelemetryTracingBridge::new(&logger_provider).with_filter(
        EnvFilter::new("info")
            .add_directive("opentelemetry=off".parse()?)
            .add_directive("hyper=off".parse()?)
            .add_directive("reqwest=off".parse()?),
    );
    let otel_traces = tracing_opentelemetry::layer()
        .with_tracer(tracer_provider.tracer(config.service_name.clone()));

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt)
        .with(otel_traces)
        .with(otel_logs)
        .try_init()?;

    Ok(Telemetry {
        tracer: Some(tracer_provider),
        meter: Some(meter_provider),
        logger: Some(logger_provider),
    })
}

use tracing_subscriber::Layer as _;

/// Reads a W3C trace context from request headers.
struct HeaderExtractor<'a>(&'a axum::http::HeaderMap);

impl Extractor for HeaderExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|v| v.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(axum::http::HeaderName::as_str).collect()
    }
}

/// The span for one HTTP request, continuing the caller's trace if it sent one. Only the
/// matched route goes in it, never the raw path, which can carry tokens.
pub fn request_span(request: &Request) -> tracing::Span {
    use tracing_opentelemetry::OpenTelemetrySpanExt as _;

    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map_or("unmatched", MatchedPath::as_str);
    let span = tracing::info_span!(
        "request",
        otel.name = format!("{} {route}", request.method()),
        otel.kind = "server",
        http.request.method = %request.method(),
        http.route = route,
        http.response.status_code = tracing::field::Empty,
    );
    let parent =
        global::get_text_map_propagator(|p| p.extract(&HeaderExtractor(request.headers())));
    let _ = span.set_parent(parent);
    span
}

/// `http.server.request.duration`, by method, route and status.
pub async fn record_request(request: Request, next: Next) -> Response {
    use std::sync::OnceLock;
    static DURATION: OnceLock<Histogram<f64>> = OnceLock::new();
    let duration = DURATION.get_or_init(|| {
        global::meter("drusite-server")
            .f64_histogram("http.server.request.duration")
            .with_unit("s")
            .with_description("Duration of HTTP server requests")
            .build()
    });

    let method = request.method().to_string();
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map_or_else(|| "unmatched".to_owned(), |p| p.as_str().to_owned());
    let start = Instant::now();
    let response = next.run(request).await;
    let status = response.status().as_u16();
    tracing::Span::current().record("http.response.status_code", status);
    duration.record(
        start.elapsed().as_secs_f64(),
        &[
            KeyValue::new("http.request.method", method),
            KeyValue::new("http.route", route),
            KeyValue::new("http.response.status_code", i64::from(status)),
        ],
    );
    response
}
