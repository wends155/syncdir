# Rust Coding Standards — Governance Core

> **Applies to:** All Rust codebases within this workspace.
> **Language Skills:** For implementation patterns, see the Language Dispatch Table below.

## 1. Version & Toolchain

| Item | Standard |
| :--- | :--- |
| **Rust Version** | Latest stable (currently **1.93.1**) |
| **Edition** | `2024` for all new projects |
| **Toolchain Manager** | `rustup` |
| **Required Components** | `rustfmt`, `clippy`, `rust-analyzer` |

> [!IMPORTANT]
> Pin `rust-version` in `Cargo.toml` to prevent accidental MSRV regressions.

## 2. Code Quality Gate (Zero-Exit Requirement)

Every PR / commit **must** pass all three gates before merge:

```sh
cargo fmt  --all -- --check   # Gate 1: Formatting
cargo clippy --all-targets --all-features -- -D warnings  # Gate 2: Linting
cargo test  --all-features    # Gate 3: Tests
sg scan                       # Gate 4: AST Linting (conditional — requires sgconfig.yml)
```

| Metric | Target |
| :--- | :--- |
| Formatting | 100% `rustfmt` compliance |
| Linting | Zero `clippy` warnings (deny mode) |
| AST Linting | Zero `ast-grep` findings (when configured) |
| Documentation | 100% of public APIs documented |
| Test coverage | 100% of public functions tested |
| Benchmarks | Critical paths benchmarked with Criterion |

## 3. Project Configuration

> 📘 **Skill:** `rust-project-setup` — See `.gemini/skills/rust-project-setup/SKILL.md`

### 3.1 `rustfmt.toml`
> 📘 **Skill:** `rust-project-setup`

### 3.2 Clippy Configuration

Use the workspace-level `[lints]` table (Rust 1.74+) for consistent configuration
across all crates. The default `clippy::all` group already covers correctness and
common-style lints — that is **sufficient as the baseline**. Layer `pedantic` as
**warnings** for guidance, and cherry-pick high-value individual lints.

> [!TIP]
> Avoid blanket `deny` on `nursery` (lints are unstable and may change between
> releases) or `cargo` (situational, better handled per-project). Promote
> individual lints to `deny` only when the team has validated they don't produce
> false positives in the codebase.

### 3.3 ast-grep Configuration
> 📘 **Skill:** `rust-project-setup`

## 4. Code Standards

### 4.1 Error Handling
- MUST use `thiserror` for library error types, `anyhow` for application-level errors.
- MUST NOT use `.unwrap()`, `.expect()`, `panic!()`, or `todo!()` in production code (enforced by ast-grep rule).
> 📘 **Patterns:** `rust-patterns` skill

### 4.2 Async / Await
- MUST set timeouts on all async operations.
- MUST use structured concurrency (JoinSet/TaskTracker).
> 📘 **Patterns:** `rust-patterns` skill

### 4.3 Memory Management & Ownership
- MUST prefer borrowing over cloning.
- MUST use `Arc` only when shared ownership across threads is required; prefer `Rc` in single-threaded code.
> 📘 **Patterns:** `rust-patterns` skill

### 4.4 Type System & API Design
- MUST use `#[must_use]` on functions whose return value should not be silently discarded.
> 📘 **Patterns:** `rust-patterns` skill

### 4.5 Documentation Standards

#### Function & Type Docs

Every public item **must** have a doc comment (`///`) that includes:

1. **Summary** — one-line description.
2. **Details** — extended explanation (if needed).
3. **`# Errors`** — documents each error variant the function can return.
4. **`# Panics`** — documents conditions under which the function panics (ideally none).
5. **`# Examples`** — runnable code example (serves as a doc-test).

#### Module Docs

Every `lib.rs`, `main.rs`, and top-level module file **must** have a `//!` doc comment providing:

- **Purpose** — what this module/crate does.
- **Key types** — the primary structs, traits, and enums it exposes.
- **Usage** — brief guidance on how consumers should use it.

### 4.6 Design Patterns & Best Practices
> 📘 **Patterns:** `rust-patterns` skill

### 4.7 Module & Workspace Organization
- MUST NOT make something `pub` just to satisfy a compiler error.
- MUST NOT create circular dependencies between crates.
- MUST NOT expose internal crate error types in your public API — wrap them.
> 📘 **Patterns:** `rust-project-setup` skill

### 4.8 Observability & Logging
- MUST use `tracing` crate (not `log`) as the standard logging framework.
- MUST use structured fields, not string interpolation.
- MUST NOT use `println!`, `eprintln!`, or `dbg!` in production code — use `tracing` macros.
> 📘 **Patterns:** `rust-observability` skill

### 4.9 Defensive Programming

Validate inputs, enforce invariants, and handle edge cases — don't assume callers will provide valid data.

**Rules:**
- Validate all public function inputs at the boundary. Return `Err` for invalid data — don't propagate it deeper.
- Use newtypes to encode validation in the type system so invalid states are unrepresentable.
- Handle all edge cases explicitly: empty collections, zero-length strings, `None`, boundary values (0, `MAX`, `MIN`).
- Prefer `.get()` over indexing (`[]`) for slices, maps, and vectors.
- Use `saturating_*` or `checked_*` arithmetic to prevent overflow/underflow.
- Fail gracefully — return meaningful errors rather than crashing. A degraded response is better than no response.

### 4.10 Environment Configuration
- MUST use centralized config struct with fail-fast validation instead of scattered `std::env::var()` calls.
- MUST NOT commit `.env` files to git (`.env.example` only).
- MUST require `.env.example` to be committed and kept up-to-date.
- MUST NOT use `Option<T>` for required configuration.
- MUST NOT hardcode URLs, ports, or credentials — all external endpoints come from config.
> 📘 **Patterns:** `rust-project-setup` skill

## 5. Testing Standards
- MUST follow TDD (Red → Green → Refactor).
- MUST cover at least: success, error response, and timeout scenarios for integration tests.
- MUST NEVER depend on local `docker compose up` for tests — tests must be self-contained.
- MUST NEVER call live external APIs in CI — all integration tests use wiremock.
- MUST NEVER use production API keys in test config.
> 📘 **Patterns:** `rust-testing` skill

## 6. Performance Benchmarking
> 📘 **Reference:** `rust-project-setup` skill

## 7. CI/CD Integration
- MUST enforce the Code Quality Gate (§2).
> 📘 **Reference:** `rust-project-setup` skill

## 8. Tools & Technologies
> 📘 **Reference:** `rust-project-setup` skill

## 9. Metrics & Monitoring
> 📘 **Reference:** `rust-project-setup` and `rust-observability` skills

## 10. Quick Reference – Prohibited Patterns

| Don't | Do Instead |
| :--- | :--- |
| `.unwrap()` in production | Use `?`, `map_err`, or `.unwrap_or_default()` |
| `println!` for logging | Use `tracing::info!` / `tracing::error!` |
| `clone()` without reason | Borrow first; clone only when ownership is needed |
| Raw `thread::spawn` | Use `tokio::spawn` with structured concurrency |
| `unsafe` without comment | Add `// SAFETY:` explaining the invariant |
| Magic numbers | Named constants or enums |
| Wildcard imports `use foo::*` | Explicit imports or re-exports |
| Mutable globals | `OnceLock`, DI, or runtime config |
| Boolean flags for state tracking | Typestate pattern |
| Manual resource cleanup calls | RAII / Drop guard |
| `struct` with 10+ constructor args | Builder pattern |
| Wrapper structs for one method | Extension trait |
| `Mutex<Option<T>>` for lazy init | `OnceLock` or `LazyLock` |
| Direct DB access in business logic | Repository pattern |
| Hard-coded dependencies | DI via Traits |
| Scattered `std::env::var` calls | Centralized config struct |
| Calling live APIs in CI tests | Wiremock or mockall |
| Shared test database state | Testcontainers per suite or transaction rollback |
| `.env` committed to git | `.env.example` only; `.env` in `.gitignore` |

---

> [!NOTE]
> `todo!()` remains prohibited. For multi-phase projects, use `// STUB(Phase N): description`
> markers instead (see `phase-rules.md §3`). Stubs must be functional code that returns Ok
> and logs a warning — never panicking placeholders.

---

## Language Dispatch Table

| Language | Skills | Load When |
|:---|:---|:---|
| **Rust** | `rust-patterns`, `rust-project-setup`, `rust-testing`, `rust-observability` | During `/build` for Rust codebases |
| **Svelte** | `svelte-patterns`, `web-fundamentals` | During `/build` for Svelte codebases |
| **TypeScript** | `typescript-patterns`, `web-fundamentals` | During `/build` for TS codebases |
| **Cross-cutting** | `knowledge-rag-query` | During `/build` when looking up dependency APIs or patterns |

---

> **Maintained by:** The Architect role (High-Reasoning Model)
> **Compliance:** All code contributions are validated against this document during the Reflect phase.
