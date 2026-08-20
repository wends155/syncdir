---
name: typescript-patterns
description: >
  Essential TypeScript coding patterns for robust type design, error handling, async usage, modularity, and API design.
---

# TypeScript Patterns

## When to Use
Load this skill when you need guidance on TypeScript type design, advanced type features (mapped types, unions), error handling with Result patterns, async execution, testing with Vitest, and optimal `tsconfig` settings.

## 1. Type System Patterns

> [!CAUTION]
> **Avoid `any` at all costs.** Opt for `unknown` if a type is genuinely indeterminable contextually, and subsequently narrow it using type guards.

**Type Patterns Checklist:**
- [ ] **Discriminated Unions**: Prefer discriminated unions for representing exclusive states (e.g., success vs. failure) leveraging a common `status` or `type` tag.
- [ ] **Branded Types**: Use branded types (newtypes) to differentiate identical primitive types like `UserId` vs `ProductId` strings.
- [ ] **Satisfies Operator**: Use `satisfies` to validate shapes without widening the inferred specific object type.
- [ ] **Template Literal & Conditional Types**: Utilize template literal types to enforce strict string formats and conditional types for dynamic inference.

---

## 2. Error Handling

**Result Pattern:**
Avoid bare `throw` statements in core or library logic, as TypeScript lacks typed `throws` signatures. Instead, return a `Result` type mirroring Rust's error handling.
- Use `{ ok: true, data: T } | { ok: false, error: E }` signatures.
- Alternatively, use libraries like `neverthrow` to bring monadic error handling into TS.
- Only throw `Error` instances for truly unrecoverable exceptions or bounded runtime panics.

---

## 3. Async Patterns

- **Concurrent Execution**: Use `Promise.allSettled` to execute concurrent tasks when independent failures shouldn't short-circuit the entire process. Use `Promise.all` only when complete success is strictly necessary.
- **Cancellation**: Implement `AbortController` and `AbortSignal` for all long-running or network-bound async tasks to allow for deterministic cancellation.
- **Timeouts**: Wrap native async calls with generic timeout wrappers using `Promise.race()` to enforce strict upper bounds on external I/O execution.

---

## 4. Module Organization

- **ESM Conventions**: Default to fully compliant ESM imports, using the `.js` extension in local absolute imports if Node16 module resolution is mandated, or utilize path aliases properly defined in `tsconfig` and the bundler config.
- **Path Aliases**: Set up path aliases (e.g. `~/*`) to keep import paths shallow and refactor-friendly.
- **Barrel Exports**: Use barrel exports (`index.ts`) selectively to expose public modules from a directory, sealing implementation details.
- **Package map**: Leverage package.json `exports` map for explicit exposure control in library crates.

---

## 5. API Design

- **Strict Signatures**: Define explicitly typed argument and return signatures on all exported functions.
- **Immutability**: Heavily utilize `Readonly<T>` and `ReadonlyArray<T>` to enforce immutability at boundaries.
- **Type Composition**: Rely on utility types like `Pick`, `Omit`, and `Partial` to derive boundary representations from domain entities, rather than manually recreating structure interfaces.
- **Runtime Validation**: Use Schema validation (e.g., `Zod`, `Valibot`) for incoming data (HTTP payloads, external dependencies) to guarantee type safety at boundary interfaces.

---

## 6. Testing

- **Vitest**: Standardize on `Vitest` for speed and modern compatibility. 
- **Type Testing**: Adopt `expectTypeOf` to test complex generic type inferences mathematically without runtime assertions.
- **Mocking**: Leverage `vi.fn()` for function spies and `vi.mock()` for module-level isolation. Limit mock scope to keep tests robust.

---

## 7. Configuration (`tsconfig.json`)

Ensure strict safety defaults to block potential edge cases:
- `"strict": true` (enables all strict type-checking options)
- `"noUncheckedIndexedAccess": true` (forces handling possible `undefined` when accessing dynamic dictionary keys or array indexes)
- `"exactOptionalPropertyTypes": true` (differentiates between missing keys and explicitly passed `undefined`)
- `"forceConsistentCasingInFileNames": true`

---

## 8. Quick Reference – Prohibited Patterns

| Don't | Do Instead |
| :--- | :--- |
| `any` | Use `unknown` and type guards |
| `@ts-ignore` | Use `@ts-expect-error` and document the exact reason |
| Bare `throw new Error()` | Return a `Result` tuple or object (`{ok: false, error}`) |
| Mutable properties as default | Use `Readonly<T>` or `as const` for boundaries |
| `enum` | Use string literal union types (`'A' \| 'B'`) |
| Unsafe array indices (`arr[i]`) | Use `.at(i)` or enable `noUncheckedIndexedAccess` |
| Deeply nested `try/catch` | Extract logic into pure functions returning `Result` |
| Exporting all internal files | Use a controlled index.ts / `exports` map |
| Using `! `(Non-null assertion) | Perform a rigorous truthy check or throw an error |

---

> **Maintained by:** The Architect role (High-Reasoning Model)
> **Compliance:** All TypeScript codebase contributions are validated against this document during the Reflect phase.
