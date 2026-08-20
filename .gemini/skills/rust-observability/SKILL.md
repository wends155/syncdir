---
name: rust-observability
description: >
  Standard logging, instrumentation, and observability practices for Rust codebases.
---

# Rust Observability

## When to Use
Load this skill when setting up logging, defining tracing spans, adjusting log levels, or implementing system metrics.

## 1. Tracing Framework

Use `tracing` as the standard logging and instrumentation framework for all Rust projects.

### 1.1 Log Levels

| Level | Use for | Example |
|:---|:---|:---|
| `error!` | Actionable failures requiring attention | DB connection lost, auth failure |
| `warn!` | Recoverable issues, degraded behavior | Retry succeeded, fallback used |
| `info!` | Milestones, lifecycle events | Server started, request completed |
| `debug!` | Internal state useful during development | Cache hit/miss, parsed config values |
| `trace!` | Verbose flow, hot-path details | Function entry/exit, loop iterations |

### 1.2 Structured Fields

Always use structured key-value fields, not string interpolation:

- Use `%` for `Display` formatting: `tracing::info!(user_id = %id, "processing")`
- Use `?` for `Debug` formatting: `tracing::debug!(config = ?cfg, "loaded")`
- Use typed fields for metrics: `tracing::info!(duration_ms = elapsed.as_millis(), "completed")`

### 1.3 Spans & Instrumentation

- Use `#[tracing::instrument]` on public functions to auto-create spans with arguments.
- Skip sensitive fields: `#[instrument(skip(password, token))]`.
- **Spans** track duration and context of an operation (a unit of work with a start and end).
- **Events** (`info!`, `error!`) are point-in-time occurrences within a span.
- Nest spans to build a call tree — each span inherits its parent's context.

### 1.4 Subscriber Setup

- Use `tracing-subscriber` with `EnvFilter` for runtime-configurable log levels.
- For JSON output (production): `tracing_subscriber::fmt().json()`.
- For human output (development): `tracing_subscriber::fmt().pretty()`.
- For OpenTelemetry integration: add `tracing-opentelemetry` as a layer.

**Rules:**
- Never use `println!`, `eprintln!`, or `dbg!` in production code — use `tracing` macros.
- Every public async function should have `#[instrument]`.
- Include request/correlation IDs in spans for distributed tracing.
- Log at `info` level for operations that help reconstruct what happened in production.

---

## 2. Metrics & Monitoring

### 2.1 Code Quality Metrics

| Metric | Target | Tool |
| :--- | :--- | :--- |
| Test coverage | >= 90% line coverage | `cargo-tarpaulin` |
| Clippy warnings | 0 | `cargo clippy` |
| Doc coverage | 100% public API | `cargo doc` |
| Benchmark regressions | < 5% | Criterion |

> [!NOTE]
> Line coverage target (>=90%) complements the Zero-Exit requirement for 100% public API
> function coverage. Both apply — every public function must be tested, and
> overall line coverage should exceed 90%.

### 2.2 Development Metrics

| Metric | Purpose |
| :--- | :--- |
| Build time | Track incremental & clean build perf |
| Dependency count | Minimize supply-chain surface |
| Security advisories | Zero unmitigated CVEs |
