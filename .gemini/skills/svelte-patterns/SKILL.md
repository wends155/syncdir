---
name: svelte-patterns
description: >
  Essential Svelte and SvelteKit coding patterns for Svelte 5 runes, state management, routing, testing, and component design.
---

# Svelte Patterns

## When to Use
Load this skill when you need guidance on Svelte 5 component design, reactive state management using runes, SvelteKit routing and data loading conventions, or testing strategies.

## 1. Component Patterns

> [!CAUTION]
> **Embrace Svelte 5 Runes** — legacy reactive statements (`$:`) and `export let` are strongly discouraged in modern codebases.
> - Use `$props()` instead of `export let`.
> - Use `$state()` for local reactivity.
> - Use `$derived()` instead of reactive bindings.
> - Use snippet blocks (`{#snippet ...}`) instead of multiple slots.

**Component Design Checklist:**
- [ ] Component is focused and adheres to single-responsibility.
- [ ] Props are defined using `$props()` and typed appropriately.
- [ ] Reusable DOM structures within the component use `{#snippet}`.
- [ ] Event handlers passed as props (e.g. `onclick`) instead of `createEventDispatcher`.

---

## 2. Reactive State Management

**Svelte 5 Runes:**
- **`$state()`** — use to declare deeply reactive mutable state.
- **`$derived()`** — use for values computed from state. Re-evaluates only when dependencies change.
- **`$effect()`** — use to synchronize state with external systems (e.g., DOM manipulation, analytics). Try to avoid using `$effect` for state-to-state synchronization; use `$derived` instead.
- **`$state.raw()`** — use for large immutable objects where deep reactivity is unnecessary overhead.

**Shared State:**
While Svelte 5 runes can be exported from `.js/.ts` files to create shared state across the app, prefer using SvelteKit's built-in `$page` store or Svelte Context (`setContext`/`getContext`) intertwined with runes to prevent state leakage on the server during SSR.

---

## 3. SvelteKit Conventions

**Routing and Data Loading:**
- File-based routing dictates the layout.
- Use `+page.server.ts` for database calls, secret environment variables, and secure operations. Return plain objects.
- Use `+page.ts` only when data fetching must happen on both client and server or relies on client-only APIs.
- Form actions (`export const actions`) should reside in `+page.server.ts` for handling `<form method="POST">`.

**Hooks and Middleware:**
- Use `hooks.server.ts` for authentication, modifying response headers, or request logging.

---

## 4. Error Handling

**SvelteKit Error Patterns:**
- Use `error(status, message)` from `@sveltejs/kit` to intentionally trigger an error boundary from page/server logic.
- Catch domain errors at the boundary and explicitly throw meaningful `error()` responses.
- Implement customized `+error.svelte` pages for user-friendly error state rendering.
- For form action failures, use the `fail(status, data)` helper to return validation errors to the client without discarding form input.

---

## 5. Styling

- Use scoped Vanilla CSS or PostCSS within `<style>` blocks.
- **TailwindCSS**: If the project uses Tailwind, prefer utility classes and `tailwind-merge` utility functions for dynamic class conditional joining.
- Avoid global styles unless absolutely necessary (defined in `app.css`). Keep styles scoped to the component.

---

## 6. Performance

- Use `{#await}` blocks for promises that resolve directly in the markup to prevent UI blocking.
- Be cautious of Over-Hydration. Optimize server payloads; only return data to the client that is necessary.
- Prefer `$effect.pre()` if you need to run synchronous logic right before the DOM updates.

---

## 7. Testing

- **Unit/Component Testing**: Use Vitest alongside `@testing-library/svelte`. Test behavior, not implementation details.
- **E2E Testing**: Playwright is the standard.
- Mocking: Isolate component testing by mocking SvelteKit stores (`$app/stores`, `$app/navigation`) using `vi.mock()`.

---

## 8. Quick Reference – Prohibited Patterns

| Don't | Do Instead |
| :--- | :--- |
| `$: computed = x * 2` | Use `$derived(x * 2)` |
| `export let myProp` | Use `let { myProp } = $props()` |
| `$effect` to update state based on state | Use `$derived` |
| `createEventDispatcher` | Pass callback functions as `$props()` |
| Slots for multiple content areas | Use `{#snippet}` blocks |
| `$page.url` reactivity via `$` | Access `$page.url` or track navigation state |
| Server logic in `+page.ts` | Move to `+page.server.ts` |
| Mutating props directly | Use bindable props or send events back up |
| `document.getElementById` | Use component `bind:this` for DOM references |
| Exposing generic server errors | Use custom `error(code, message)` |

---

> **Maintained by:** The Architect role (High-Reasoning Model)
> **Compliance:** All Svelte codebase contributions are validated against this document during the Reflect phase.
