---
name: scaffold-plan
description: >
  Generate the implementation plan skeleton from the ipr.md format, manage plan revisions, resolve builder decisions, and handle safe session resumption.
---

# Scaffold Plan Lifecycle

## When to Use
Load this skill when you are the Architect tasked with drafting a new Implementation Plan during the Think Phase (`/plan-making` workflow), revising an existing plan, resolving a design decision during the Reflect Phase (`/audit` workflow), or resuming an active implementation cycle across sessions.

## A. Scaffold Generation
*Sourced from ipr.md*

When creating a new plan:
1. **Target:** Read `.agents/rules/ipr.md` to load the definitive structural format.
2. **Tier Decision:** Determine the scaling tier (S, M, L) based on the user's scope analysis.
3. **Extract & Populate:** Mechanically extract the required section headers and template structures from the `.agents/rules/ipr.md` specific definition block. Pre-populate the plan with field placeholders (e.g., `<insert detail>`). Do not omit mandatory headers for your chosen tier.
4. **Vocabulary:** Ensure the Pre/Post Vocabulary table and CHECKPOINT frequency conventions are included.

## B. Revise (Revision Protocol)
*Absorbed from ipr.md §3*

When revising an active Implementation Plan based on user feedback or architectural pivots:
1. **Target Edits:** Use `replace_file_content` or `multi_replace_file_content` tools. Never rewrite the entire plan—this causes massive token churn and risks silent loss of untouched sections.
2. **Tag Changes:** Prepend `[REVISED]` to the header or content of the changed section so the exact delta is highly visible to the user and Builder.
3. **Preserve Context:** Never condense or summarize unchanged content. If a section was not discussed, do not touch it.
4. **Task Sweep:** After any revision, you must explicitly re-run the Validate `task.md` procedure (from the `/plan-making` workflow) to ensure the Builder’s task tracking remains perfectly aligned with the newly revised constraints.

## C. Resolve Decisions
*Absorbed from ipr.md §4*

When the user approves one solution among multiple options, the plan must instantly transition from a "proposal" state to an "execution" state:
1. **Clean Slate:** Delete the rejected options entirely from the plan. Do not leave them lingering as "Rejected: Option B."
2. **State Fact:** Restate the chosen option as authoritative fact. (e.g., Change "Option A: Use `tokio::spawn`" to "The implementation uses `tokio::spawn`.")
3. **Log Rationale:** Move the structural reasoning and tradeoffs into the plan’s *Problem Statement* section, or persist them within `context.md` for historical traceability.
4. **Sanitize Constraints:** Use a tool block to run a `grep_search` for `Option A|Option B|Alternative|vs\.` across the plan file to sweep for stray comparative language.

## D. Resume (Resumption Protocol)
*Absorbed from ipr.md §8*

When a new session targets an already-in-progress plan (typically during the `/build` workflow):
1. **Read Context:** Parse `context.md` for the explicit prior session summarize to understand where you left off.
2. **Read Implementation State:** Read `task.md` and identify the specific transition point between completed items (`[x]`) and pending items (`[ ]`). 
3. **Verify Integrity:** Before writing any new code, execute the project's quality verification pipeline (Fmt + Lint + Test) to confirm previous work compiles and passes all checks.
4. **Start Execution:** Resume execution beginning at the strict first pending `[ ]` step. Absolutely do not attempt to re-execute or modify steps marked `[x]`. 
5. **Circuit Breaker:** If the verification pipeline fails on the prior session's work, immediately STOP. Generate an error report and escalate. Do not silently attempt to fix past steps out-of-band.

## E. Scaffold Templates

<!-- TEMPLATE_START: S-Tier -->
**Role:** Architect • **Date:** YYYY-MM-DD • **Tier:** S
**Scope:** <One-line summary>

### Problem Statement
<Constraint and dependency details>

### Global Execution Order
Step 1: [TEST|NEW|MODIFY|DELETE] filepath — [+|~|-] symbol_name (L##-##)
- Pre: ALL
- Target: function/struct name + line range
- Action: <action description or code snippet>
- Post: GREEN(test_name) / ALL 🔒

### Verification Plan
| Type | Command |
|------|---------|
| Tests | <command> |
| Lint | <command> |

### Plan Summary
| Metric | Value |
|--------|-------|
| Tier | S |
| Files | N |
| Steps | N |
| Checkpoints | N |
| Estimated effort | Low |
<!-- TEMPLATE_END: S-Tier -->

<!-- TEMPLATE_START: M-Tier -->
**Role:** Architect • **Date:** YYYY-MM-DD • **Tier:** M
**Scope:** <One-line summary>

### Builder Context
Read before starting:
- `path/to/file` (reason)

### Phase Context *(Optional)*
- **Phase:** N of M
- **Prior phase:** <summary>
- **Stubs for this phase:** STUB(Phase N)

### Problem Statement
<Constraint and dependency details>

### Negative Scope
**Out of Scope**
- Do NOT touch <file or area>

### Interface Contracts
<New or changed public signatures, invariants, errors>

### Blast Radius Table
| Symbol | File | Direct Callers | Indirect Deps | Test-Only | Cross-Package? |
|--------|------|---------------|---------------|-----------|----------------|
| `fn()` | `file` | 0 | 0 | 0 | No |

### Deprecation Schedule *(Optional)*
| Old Symbol | New Symbol | Introduced | Removal Target | Migration |
|-----------|-----------|-----------|---------------|-----------|
| `old()` | `new()` | vX | vY | <guide> |

### Edge Cases & Risks
<Document edge cases>

### Test Plan (TDD)
1. **Test cases**: <code assertions>
2. **Test type**: <Unit/Integration>
3. **Expected failures**: <what breaks first>
4. **Location**: <file path>

### Global Execution Order
Step 1: [TEST|NEW|MODIFY|DELETE] filepath — [+|~|-] symbol_name (L##-##)
- Pre: ALL
- Target: function/struct name + line range
- Action: <action description or code snippet>
- Post: GREEN(test_name) / ALL 🔒

### Verification Plan
| Type | Command |
|------|---------|
| Tests | <command> |
| Lint | <command> |

### Plan Summary
| Metric | Value |
|--------|-------|
| Tier | M |
| Files | N |
| Steps | N |
| Checkpoints | N |
| Estimated effort | Medium |
<!-- TEMPLATE_END: M-Tier -->

<!-- TEMPLATE_START: L-Tier -->
**Role:** Architect • **Date:** YYYY-MM-DD • **Tier:** L
**Scope:** <One-line summary>

### Builder Context
Read before starting:
- `path/to/file` (reason)

### Phase Context *(Optional)*
- **Phase:** N of M
- **Prior phase:** <summary>
- **Stubs for this phase:** STUB(Phase N)

### Problem Statement
<Constraint and dependency details>

### Negative Scope
**Out of Scope**
- Do NOT touch <file or area>

### Interface Contracts
<New or changed public signatures, invariants, errors>

### Module Boundaries
- **Owns**: <responsibilities>
- **Does NOT own**: <delegations>

### Cross-Module Handshakes
- Caller → Callee: <details>
- Data format: <details>
- Error path: <details>

### Architecture Diagram
```mermaid
graph TD;
    A-->B;
```

### Blast Radius Table
| Symbol | File | Direct Callers | Indirect Deps | Test-Only | Cross-Package? |
|--------|------|---------------|---------------|-----------|----------------|
| `fn()` | `file` | 0 | 0 | 0 | No |

### Deprecation Schedule *(Optional)*
| Old Symbol | New Symbol | Introduced | Removal Target | Migration |
|-----------|-----------|-----------|---------------|-----------|
| `old()` | `new()` | vX | vY | <guide> |

### Dependency Chain
1 → 2 → 3

### Parallel Execution Lanes *(Optional — Multi-Agent only)*
- **Lane syntax**: Group steps into named lanes, each scoped to a module boundary:
  ```
  Lane A (Steps N-M): `src/module_a/` — description
  Lane B (Steps N-M): `src/module_b/` — description
  🔒 SYNC CHECKPOINT — integrate and verify all lanes
  ```

### Edge Cases & Risks
<Document edge cases>

### Test Plan (TDD)
1. **Test cases**: <code assertions>
2. **Test type**: <Unit/Integration>
3. **Expected failures**: <what breaks first>
4. **Location**: <file path>

### Global Execution Order
Step 1: [TEST|NEW|MODIFY|DELETE] filepath — [+|~|-] symbol_name (L##-##)
- Pre: ALL
- Target: function/struct name + line range
- Action: <action description or code snippet>
- Post: GREEN(test_name) / ALL 🔒

### Verification Plan
| Type | Command |
|------|---------|
| Tests | <command> |
| Lint | <command> |

### Plan Summary
| Metric | Value |
|--------|-------|
| Tier | L |
| Files | N |
| Steps | N |
| Checkpoints | N |
| Estimated effort | High |
<!-- TEMPLATE_END: L-Tier -->

<!-- TEMPLATE_START: task.md -->
# Task: <plan title>

## Objectives
- [ ] <Component 1>
  - [ ] [ACTION] <filename>
- [ ] Run verification pipeline
- [ ] Update docs
- [ ] Update context.md
- [ ] Commit

## Lane Progress *(Optional — Multi-Agent only)*
- [ ] Lane A: `build/lane-a` -> [task_lane_a.md](file:///absolute/path/to/task_lane_a.md)
- [ ] Lane B: `build/lane-b` -> [task_lane_b.md](file:///absolute/path/to/task_lane_b.md)

## Builder Notes
- 💡 Add notes here
<!-- TEMPLATE_END: task.md -->


