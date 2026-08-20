---
name: rust-project-setup
description: >
  Rules for structuring projects, workspaces, environments, tooling, and benchmarking in Rust.
---

# Rust Project Setup

## When to Use
Load this skill when setting up a new Rust project, modifying a workspace structure, configuring cargo features/tools, setting up environment variables, or integrating CI/CD checks.

## 1. Project Configuration

### 1.1 `rustfmt.toml`

```toml
edition = "2024"
max_width = 100
tab_spaces = 4
newline_style = "Unix"
use_small_heuristics = "Default"
imports_granularity = "Crate"
group_imports = "StdExternalCrate"
```

### 1.2 Clippy Configuration

Use the workspace-level `[lints]` table (Rust 1.74+) for consistent configuration
across all crates. The default `clippy::all` group already covers correctness and
common-style lints — that is **sufficient as the baseline**. Layer `pedantic` as
**warnings** for guidance, and cherry-pick high-value individual lints.

> [!TIP]
> Avoid blanket `deny` on `nursery` (lints are unstable and may change between
> releases) or `cargo` (situational, better handled per-project). Promote
> individual lints to `deny` only when the team has validated they don't produce
> false positives in the codebase.

```toml
# Cargo.toml (workspace root)
[workspace.lints.clippy]
# ── Baseline (default) ────────────────────────────────────────
all = "deny"                          # correctness + common style

# ── Guidance ──────────────────────────────────────────────────
pedantic = "warn"                     # stricter style — warn, don't block

# ── Cherry-picked high-value lints (deny) ─────────────────────
missing_errors_doc       = "deny"     # every Result-returning fn must document errors
missing_panics_doc       = "deny"     # every potentially-panicking fn must document it
undocumented_unsafe_blocks = "deny"   # enforce // SAFETY: comments
cast_possible_truncation = "deny"     # catch lossy integer casts
large_futures            = "deny"     # prevent accidentally huge futures on the stack

# ── Useful pedantic lints relaxed to allow (override as needed) ─
module_name_repetitions  = "allow"    # common in domain-driven designs
must_use_candidate       = "allow"    # too noisy for general use

[workspace.lints.rust]
unsafe_code              = "warn"     # highlight unsafe usage without hard-blocking

[lints]
workspace = true
```

#### Recommended Per-Project Additions

Enable these when they apply to your project:

| Lint | Level | When to enable |
| :--- | :--- | :--- |
| `clippy::nursery` | `warn` | Opt-in for experimental early warnings |
| `clippy::cargo` | `warn` | When publishing crates to crates.io |
| `clippy::missing_docs_in_private_items` | `warn` | For library-heavy projects needing internal docs |
| `clippy::unwrap_used` | `deny` | For production services (not tests) |
| `clippy::expect_used` | `warn` | Pair with `unwrap_used` for stricter error handling |
| `clippy::indexing_slicing` | `warn` | For safety-critical code avoiding panics |

### 1.3 ast-grep Configuration

AST-aware linting enforces structural patterns that `clippy` cannot cover (e.g., path-scoped rules like "no DB queries outside `repo` modules"). 

The workspace provides starter rule templates in `.ast-grep/`. Downstream projects should adopt these by copying `sgconfig.yml` to their project root.

| Rule ID | Enforces | Severity | Customization |
| :--- | :--- | :--- | :--- |
| `scattered-env-var` | Centralized config | Warning | Adjust `ignores` for your config module path |
| `block-on-in-async` | Async deadlocks | Error | - |
| `raw-thread-spawn` | Structured concurrency | Warning | - |
| `hardcoded-url` | Centralized config | Hint | - |
| `sqlx-outside-repo` | Repository pattern | Warning | Adjust `ignores` for your infra modules |
| `mutex-option-antipattern` | Interior mutability | Hint | - |
| `arc-vec-antipattern` | Memory management | Warning | - |
| `raw-tokio-spawn` | Async best practices | Hint | Add your startup/background files to `ignores` |
| `wildcard-import` | Re-exports & Prelude | Warning | - |
| `missing-instrument` | Observability | Hint | Add your test files to `ignores` |
| `scattered-dotenv` | Environment Config | Warning | Add your startup/config files to `ignores` |
| `unwrap-in-production` | Error Handling | Warning | Add project `tests/**` paths to `ignores` if different |

---

## 2. Module & Workspace Organization

Rules for structuring multi-module, multi-crate Rust projects.

### 2.1 Visibility

Minimize public surface area. Default to private; widen only when needed.

| Visibility | Use when |
|:---|:---|
| (private) | Implementation detail — no outside access needed |
| `pub(super)` | Parent module needs access (e.g., test helpers) |
| `pub(crate)` | Other modules within the same crate need access |
| `pub` | Part of the crate's public API, documented and stable |

> [!CAUTION]
> **Never** make something `pub` just to satisfy a compiler error. If a type is needed
> across crate boundaries, re-evaluate the crate boundary — it may be in the wrong place.

### 2.2 Workspace Organization

| Criterion | Same crate (modules) | Separate crate |
|:---|:---|:---|
| Shared types/traits | Same crate | Extract a `*-core` or `*-types` crate |
| Independent release cycle | N/A | Separate crate |
| Build time isolation | Same crate | Separate crate (parallel compilation) |
| Feature-gated functionality | Feature flags within crate | Separate crate |
| Shared test utilities | `#[cfg(test)]` module | Dedicated `*-test-utils` crate |

**Conventions:**
- Name crates `<project>-<role>` (e.g., `tars-core`, `tars-mcp`, `tars-cli`).
- Put shared types in a `-core` or `-types` crate that others depend on.
- Never create circular dependencies between crates.

### 2.3 Cross-Crate Error Strategy

Each crate defines its own error type with `thiserror`. At crate boundaries, convert foreign errors into local error variants using `#[from]` and `map_err`.

**Rules:**
- Never expose internal crate error types in your public API — wrap them.
- For workspaces: a `-core` crate may define shared error types that all crates use as a base.

### 2.4 Re-exports & Prelude

Curate the public API surface from `lib.rs` using `pub use`:

**Rules:**
- Re-export the primary types, traits, and error types from `lib.rs`.
- Create a `prelude` module for types users will import in nearly every file.
- Keep prelude small — only the most frequently used items.
- Internal modules should be `pub(crate)`, not `pub`, unless they're part of the API surface.

### 2.5 Feature Flags

Use Cargo features for optional functionality within a crate:

**Rules:**
- Default features should cover the common use case.
- Feature names should be descriptive: `serde`, `async`, `cli`, not `feat1`.
- Gate expensive dependencies behind features (e.g., `tracing`, `serde`).
- Document all features in `Cargo.toml` with inline comments.
- Test with `--all-features` AND with no features to catch compilation issues.

### 2.6 Module Communication

Modules communicate through well-defined interfaces, not by reaching into each other's internals. Apply Dependency Injection via Traits for all cross-module dependencies.

**Rules:**
- Data flows through shared types from a `-core` crate, not through direct module imports.
- If module A needs to call module B, A depends on B's **trait**, not B's **struct**.
- Use events or channels for decoupled communication between modules that shouldn't know about each other.

---

## 3. Environment Configuration

Centralize configuration loading and validation to prevent runtime surprises.

### 3.1 File Hierarchy

| File | Committed? | Purpose |
|:---|:---:|:---|
| `.env.example` | ✅ Yes | Template with all keys, placeholder values, and comments |
| `.env` | ❌ No (gitignored) | Local development values — real API keys for dev |
| `.env.test` | ⚠️ Optional | Test-safe values (no real API keys, localhost URLs) |
| Production | N/A | Real env vars injected by deployment platform |

> [!IMPORTANT]
> `.env.example` must be committed and kept up-to-date. It serves as documentation
> for every configuration key the application requires. Never put real secrets in it.

### 3.2 Config Struct Pattern

All configuration must flow through a typed, validated struct:

- Parse from environment at application startup using `dotenvy` + `std::env::var`
- Fail fast — if a required variable is missing, the application must exit immediately with a
  clear error message naming the missing variable
- Use newtypes for validated config values (e.g., `DatabaseUrl`, `ApiKey`, `Port`)
- Use an `Environment` enum (`Dev`, `Staging`, `Prod`) to control behavior differences

**Rules:**
- Never scatter `std::env::var()` calls throughout the codebase — read all env vars once into
  the config struct at startup
- Never use `Option<T>` for required configuration — if it's required, fail at startup, not at
  first use
- Never hardcode URLs, ports, or credentials — all external endpoints come from config
- Load `.env` only in dev/test — production uses real env vars
- Use `#[cfg(test)]` or a test-specific config constructor for test environments

### 3.3 Gitignore Requirements

Every project with environment configuration must include in `.gitignore`:

```
.env
.env.local
.env.*.local
```

> [!CAUTION]
> If `.env` is accidentally committed, rotate ALL secrets immediately.
> `git rm --cached .env` removes it from tracking, but the secrets are
> already in git history.

---

## 4. Performance Benchmarking

Use **Criterion** for all performance-sensitive code paths. Use `criterion::black_box` to prevent the optimizer from eliding work. For async benchmarks, use `.to_async(&runtime)` on the bencher. Benchmark critical paths and track regressions against baselines.

---

## 5. CI/CD Integration

CI/CD pipeline configuration is project-specific. Define it in `architecture.md § Toolchain`.

---

## 6. Tools & Technologies

### 6.1 Development

| Tool | Purpose |
| :--- | :--- |
| `rustup` | Toolchain management |
| `rustfmt` | Code formatting |
| `clippy` | Linting & code analysis |
| `rust-analyzer` | Language server / IDE support |
| `cargo` | Build, test, package management |

### 6.2 Testing & Benchmarking

| Tool | Purpose |
| :--- | :--- |
| `cargo test` | Built-in unit & integration tests |
| `criterion` | Statistical benchmarking |
| `proptest` | Property-based / fuzz testing |
| `mockall` | Mocking framework |
| `testcontainers` | Ephemeral Docker containers for integration tests |
| `wiremock` | HTTP API mocking for integration tests |
| `cargo-tarpaulin` | Code coverage |

### 6.3 Infrastructure

| Tool | Purpose |
| :--- | :--- |
| `docker-compose` | Local development services (Postgres, Redis, etc.) |
| `dotenvy` | `.env` file loading |
| `sqlx` | Async SQL with compile-time checked queries |
| `refinery` | Database migration management |

### 6.4 Quality & Security

| Tool | Purpose |
| :--- | :--- |
| `cargo audit` | Security vulnerability scanning |
| `ast-grep` | AST-aware pattern linting (Clippy gaps) |
| `cargo outdated` | Dependency staleness check |
| `cargo tree` | Dependency graph visualization |
| `cargo expand` | Macro expansion debugging |
| `cargo deny` | License & advisory policies |
