---
name: rust-testing
description: >
  Standards and workflows for testing Rust code.
---

# Rust Testing

## When to Use
Load this skill when you need guidance on writing unit tests, integration tests (with testcontainers or wiremock), property-based tests, or configuring test environments.

## 1. TDD Flow

Follow the **Red → Green → Blue** cycle:

1. **Red** — Write a failing test that specifies the desired behavior.
2. **Green** — Write the minimum code to make the test pass.
3. **Blue** — Refactor for clarity and performance while keeping tests green.

## 2. Unit Tests

Tests follow the **Arrange-Act-Assert** pattern. Co-locate unit tests in a `#[cfg(test)] mod tests` block within the same file. Use `#[tokio::test]` for async tests. Name tests descriptively: `<action>_<scenario>_<expected>` (e.g., `process_invalid_format_returns_error`).

## 3. Integration Tests

Place integration tests in a top-level `tests/` directory. Each file in `tests/` is compiled as a separate crate — it can only access the public API.

**General Rules:**
- One file per feature area (e.g., `tests/auth.rs`, `tests/pipeline.rs`).
- Use shared fixtures via a `tests/common/mod.rs` helper module.
- Integration tests should exercise real module interactions, not mock everything.
- For tests requiring external services (DB, HTTP), use `testcontainers` or `wiremock`.

### 3.1 Database Tests (Testcontainers)

Use `testcontainers` to spin up ephemeral database containers for integration tests. Each test suite gets a fresh, isolated database — no shared state, no dependency on local infrastructure.

**Pattern — `TestDb` shared fixture:**

Create a reusable `TestDb` struct in `tests/common/mod.rs` that manages the container lifecycle:
- Start a Postgres container via `testcontainers::runners::AsyncRunner`
- Run migrations automatically (via `sqlx::migrate!()` or `refinery`)
- Provide a connection pool to the test
- Container is dropped (and destroyed) when the test ends

**Rules:**
- Each test suite gets its own container — no sharing between test files
- Migrations run before every suite — tests always start with a known schema
- Never depend on local `docker compose up` for tests — tests must be self-contained
- Use `#[tokio::test]` for async database tests
- Keep a `docker-compose.yml` in the project root for **local development only** (committed, documented in `architecture.md`)

**Docker Compose (for local dev only, not tests):**

```yaml
# docker-compose.yml — local development services
services:
  postgres:
    image: postgres:17
    environment:
      POSTGRES_DB: myapp_dev
      POSTGRES_USER: dev
      POSTGRES_PASSWORD: dev
    ports:
      - "5432:5432"
    volumes:
      - pgdata:/var/lib/postgresql/data

volumes:
  pgdata:
```

> [!CAUTION]
> `docker-compose.yml` is for developer convenience — `cargo test` must never require
> `docker compose up`. If tests need a database, they spin up their own via testcontainers.

### 3.2 External API Tests (Wiremock)

Use `wiremock` to simulate external HTTP APIs in integration tests. This enables testing
HTTP interaction patterns (headers, status codes, timeouts) without calling live services.

**Three-Layer Testing Strategy:**

| Layer | Tool | What It Tests | When to Use |
|:---|:---|:---|:---|
| **Unit** | `mockall` | Business logic in isolation | Always — every function |
| **Integration** | `wiremock` | HTTP request/response contracts | When code makes HTTP calls |
| **E2E / Sandbox** | Real API (dev endpoint) | Full end-to-end integration | Manual / staging only |

**Pattern — Wiremock MockServer:**

- Create a `wiremock::MockServer::start()` in the test setup
- Mount response mocks for expected endpoints
- Pass the mock server's URI to the client under test (via config or constructor)
- Assert that the mock was called with expected request properties

**Response Scenarios to Cover:**

| Scenario | Response | Why It Matters |
|:---|:---|:---|
| Success | 200 + valid JSON body | Happy path |
| Client error | 400/422 + error body | Input validation feedback |
| Auth failure | 401/403 | Token expiry, permission checks |
| Not found | 404 | Missing resource handling |
| Rate limit | 429 + `Retry-After` header | Backoff logic |
| Server error | 500/503 | Retry and fallback behavior |
| Timeout | No response (delay) | Timeout handling and circuit breaking |

**Rules:**
- Every external API must be behind a trait — the trait enables both mockall (unit) and wiremock (integration) testing
- Integration tests must cover at least: success, error response, and timeout scenarios
- Never call live external APIs in CI — all integration tests use wiremock
- Document the response contract (status codes, headers, body shape) in the trait's doc comment
- Use `wiremock::matchers` to verify request method, path, headers, and body — not just the response

> [!TIP]
> For APIs with complex auth flows (OAuth, JWT), create a dedicated `MockAuthServer`
> test fixture that handles token issuance and validation.

### 3.3 Test Environment Configuration

Separate test configuration from production configuration to prevent accidental
use of real credentials in tests.

**Rules:**
- Create a test-specific config constructor (e.g., `AppConfig::for_test()`) that uses safe defaults
- Use `.env.test` for integration test environment variables when needed
- Test timeouts should be shorter than production (e.g., 5s vs 30s) to catch slow tests early
- Test databases use either testcontainers (preferred) or a `_test`-suffixed database name
- Never use production API keys in test config — use wiremock or sandbox keys
- Load test config via `#[cfg(test)]` module or a test helper function

## 4. Property-Based Tests

Use `proptest` for invariant checking on complex transformations. The test generates random inputs matching a pattern and asserts that invariants hold for all of them. Acceptable error variants should be explicitly matched; unexpected errors fail the test.

## 5. Testing Checklist

- [ ] Every function has at least one happy-path and one error-path test.
- [ ] Async functions are tested with `#[tokio::test]`.
- [ ] Edge cases (empty input, max values, unicode) are covered.
- [ ] Property-based tests exist for complex transformations.
- [ ] Integration tests cover cross-module interactions.
- [ ] Mocks (`mockall`) are used for external dependencies.
- [ ] Doc-tests compile and pass (`cargo test --doc`).
- [ ] External APIs are tested with wiremock (HTTP) or mockall (trait) — not called live in CI.
- [ ] Database tests use testcontainers or isolated test databases.
- [ ] `.env.example` is committed and up-to-date with all required keys.
