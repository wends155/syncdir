---
name: rust-patterns
description: >
  Essential Rust coding patterns for error handling, async/await, memory management, the type system, and structural design patterns.
---

# Rust Patterns

## When to Use
Load this skill when you need guidance on Rust error handling, async usage, memory management, type API design, or applying design patterns (e.g., Builder, Typestate, RAII, Enum Dispatch, Repository, Dependency Injection).

## 1. Error Handling

> [!CAUTION]
> **No Crashes** — avoid all patterns that cause uncontrolled program termination:
> - `.unwrap()` / `.expect()` — reserved for tests and provably-infallible cases (with comment).
> - `panic!()` / `unreachable!()` — use error propagation instead. If truly unreachable, add a comment.
> - `todo!()` — never in production; use compile-time `#[cfg]` gates or return `Err(Unimplemented)`.
> - Index out-of-bounds (`vec[i]`) — use `.get(i)` and handle `None`.

> [!NOTE]
> **Enforcement — `unwrap-in-production`:** The `ast-grep` rule automatically excludes `.unwrap()` that live inside `mod tests { ... }` blocks (via `inside: mod_item` pattern matching) and dedicated test files (`tests/**`, `**/*test*`). Severity is `warning`.

**`thiserror` vs `anyhow`:**
- **`thiserror`** — for library error types. Define structured, matchable error enums with `#[derive(Error)]`. Each variant gets a descriptive `#[error("...")]` message. Use `#[from]` for transparent conversions from source errors.
- **`anyhow`** — for application-level error propagation. Use `anyhow::Result` when you don't need callers to match on specific variants. Add context with `.context("what failed")`.

**Error Handling Checklist:**

- [ ] Every error communicates **what**, **where**, and **why**.
- [ ] No silent failures — all `Result` values are propagated or logged.
- [ ] Errors are contextually wrapped at module boundaries.
- [ ] `#[from]` is used for transparent conversions; manual `map_err` for added context.

---

## 2. Async / Await Best Practices

Wrap external I/O in `tokio::time::timeout`. Chain `.map_err()` to convert timeout and network errors into domain error types. Always propagate errors with `?` rather than swallowing them.

**Async Rules:**

- Prefer `tokio` as the async runtime for all server-side work.
- Always set timeouts on external I/O (network, file, IPC).
- Use `tokio::select!` for concurrent branch cancellation, **not** manual `JoinHandle` polling.
- Avoid `block_on` inside async contexts — it will deadlock the runtime.
- Use structured concurrency (`JoinSet`, `TaskTracker`) over raw `tokio::spawn`.

---

## 3. Memory Management & Ownership

**Ownership Rules:**

- Prefer borrowing (`&T`, `&mut T`) over cloning.
- Use `Arc` only when shared ownership across threads is required; prefer `Rc` in single-threaded code.
- For shared mutable caches, use `Arc<RwLock<HashMap>>` or `dashmap`.
- Use `Cow<'_, str>` when a function may or may not need to allocate.
- Avoid `Box<dyn Trait>` when generics (`impl Trait` or `<T: Trait>`) suffice.
- Use `Arc<[T]>` instead of `Arc<Vec<T>>` for immutable shared slices.

---

## 4. Type System & API Design

- **Newtype pattern**: Wrap primitive types to add semantic meaning and prevent misuse. Validate in the constructor, return `Result`. Once constructed, the type guarantees validity.
- **Builder pattern**: Use for structs with many optional fields (see § 5.1).
- **Typestate pattern**: Encode valid state transitions in the type system (see § 5.2).
- **`#[must_use]`**: Apply to functions whose return value should not be silently discarded.
- **`#[non_exhaustive]`**: Apply to public enums and structs that may grow.
- **Sealed traits**: Use the sealed-trait pattern for traits not intended for external implementation.
- See § 5 for the complete design patterns reference and selection guide.

---

## 5. Design Patterns & Best Practices

Rust's type system enables patterns that catch entire categories of bugs at compile time.
This section codifies the patterns we use most, with guidance on when and why to apply each.

### 5.1 Builder Pattern

Use when constructing structs with many fields — especially when some are optional, have
defaults, or require validation.

Prefer the `bon` or `typed-builder` crates for derive-based builders. Fall back to a manual builder only when you need custom validation logic during construction. Manual builders follow the pattern: optional fields stored as `Option<T>`, chainable setter methods returning `Self`, and a `build()` method that validates and returns `Result<T, ValidationError>`.

---

### 5.2 Typestate Pattern

Encode protocol steps or lifecycle phases into the type system so that **invalid state
transitions are compile-time errors**. Use zero-sized marker types as generic parameters — no runtime cost. Each state gets its own `impl` block, so only valid operations are available at each phase.

**When to use:**
- Protocol handshakes (connect → auth → ready).
- Build pipelines (configure → validate → execute).
- File I/O (open → write → flush → close).

---

### 5.3 RAII & Drop Guards

Use Rust's `Drop` trait to guarantee resource cleanup runs automatically when a value goes out of scope — even on early returns or panics. The guard pattern: hold a reference to the resource plus a `committed` flag; on drop, if uncommitted, perform cleanup (e.g., rollback).

**Common RAII use cases:**

| Resource | Guard / Type | Cleanup Action |
|:---|:---|:---|
| Database transaction | `TransactionGuard` | Rollback on drop |
| Temp file / directory | `tempfile::TempDir` | Delete on drop |
| Mutex lock | `MutexGuard` | Release on drop |
| Timer / span | `tracing::span::Entered` | Record elapsed on drop |
| File lock | `fs2::FileLock` | Release on drop |

> [!TIP]
> For ad-hoc guards without a dedicated struct, use the `scopeguard` crate (`defer! { cleanup(); }`).

---

### 5.4 Extension Traits

Add domain-specific methods to types you don't own (e.g., `std`, `serde_json`) without violating the orphan rule. Define a trait, implement it for the foreign type, and re-export it.

**Rules:**
- Name the trait `<Type>Ext` (e.g., `StrExt`, `ResultExt`, `IteratorExt`).
- Keep extension traits in a dedicated `ext` module.
- Re-export from the crate prelude if they're used widely.

---

### 5.5 Interior Mutability

Use interior mutability when you need to mutate data behind a shared reference (`&T`).
Choose the narrowest primitive that satisfies your requirements:

| Type | Thread-safe? | Checked at | Use when |
|:---|:---|:---|:---|
| `Cell<T>` | No | Compile time | `T: Copy`, single thread, simple swap/replace |
| `RefCell<T>` | No | Runtime | Single thread, need `&mut T` borrows |
| `Mutex<T>` | Yes | Runtime | Multi-thread, exclusive write access |
| `RwLock<T>` | Yes | Runtime | Multi-thread, many readers / rare writers |
| `OnceLock<T>` | Yes | Runtime | Write-once lazy initialization |
| `Atomic*` | Yes | Lock-free | Counters, flags, simple numeric state |

> [!CAUTION]
> Prefer `OnceLock` (std) or `LazyLock` over hand-rolled `Mutex<Option<T>>`
> for lazy initialization. It's safer and more readable.

---

### 5.6 Enum Dispatch vs Trait Objects

Choose between compile-time (`enum`) and runtime (`dyn Trait`) polymorphism based on
whether the set of variants is **closed** or **open**.

| Criterion | Enum dispatch | Trait objects (`dyn Trait`) |
|:---|:---|:---|
| Variant set | Closed (known at compile time) | Open (extensible by consumers) |
| Performance | Monomorphized, inlineable | Vtable indirection |
| Pattern matching | Exhaustive `match` | Not available |
| Object safety needed? | No | Yes |
| Binary size | Larger (monomorphization) | Smaller |

Use enum dispatch for closed sets of known variants. Use trait objects for open, plugin-style extensibility. When the set is closed but you want trait syntax, consider the `enum_dispatch` crate.

---

### 5.7 Repository Pattern

Centralize data access behind a trait (e.g., `trait UserRepo`). Modules depend on the trait, not the database directly. This prevents scattered DB access across modules, makes testing trivial (mock the trait), and makes storage changes a single-point refactor.

**Rules:**
- One trait per aggregate root or domain entity.
- Methods return domain types, not raw DB rows.
- Implementations live in a dedicated `infra` or `persistence` module.
- Errors are domain errors, not raw `sqlx::Error` — map at the boundary.

---

### 5.8 Dependency Injection via Traits

The general principle that makes Repository and other patterns testable. Define behavior as a trait, accept `impl Trait` or `&dyn Trait` in struct constructors. In production, pass the real implementation. In tests, pass a mock (`mockall::automock` or a manual stub).

**Applies to:**
- Database access (Repository)
- HTTP clients
- File system operations
- Email/notification senders
- Clock/time providers

**Rules:**
- Define the trait in the consuming module (not the implementing module).
- Use `#[mockall::automock]` on the trait for automatic mock generation.
- Prefer `impl Trait` in function args, `Box<dyn Trait>` or generics in struct fields.

---

### 5.9 Pattern Selection Guide

| Problem | Recommended Pattern | Ref |
|:---|:---|:---|
| Many optional constructor fields | Builder | § 5.1 |
| Compile-time state machine enforcement | Typestate | § 5.2 |
| Guaranteed resource cleanup | RAII / Drop Guard | § 5.3 |
| Add methods to types you don't own | Extension Trait | § 5.4 |
| Mutation behind `&T` | Interior Mutability | § 5.5 |
| Known, closed set of behaviors | Enum Dispatch | § 5.6 |
| Open, extensible set of behaviors | Trait Objects (`dyn`) | § 5.6 |
| Prevent primitive type misuse | Newtype | § 4 |
| Restrict external trait implementations | Sealed Trait | § 4 |
| Scattered data access across modules | Repository | § 5.7 |
| Testable external dependencies | DI via Traits | § 5.8 |
