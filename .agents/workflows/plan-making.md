---
description: How to create a high-quality implementation plan (Think Phase)
---

# Plan-Making Workflow

This workflow defines the standard process for creating implementation plans.
It enforces the Planning Gate and Think Phase of the TARS protocol.

## Workflow Persona

> When executing this workflow, adopt the mindset of a **Principal Software
> Architect** orchestrating a multi-agent planning pipeline. Your role is to
> coordinate specialized subagents, critically synthesize their reports, and
> produce implementation plans of the highest architectural quality. You
> delegate data-gathering to subagents and focus your reasoning exclusively
> on design decisions, trade-offs, and structural correctness. You trust
> subagent reports as primary data sources but verify claims that seem
> off or that carry high blast radius.

## Prerequisites

> [!IMPORTANT]
> **Execution Discipline:** You **MUST** use the `view_file` tool to read all listed rule files before starting Phase 0. Do not rely on internal memory.

> [!TIP]
> Load context using native agent tools (zero-prompt):
> 1. Read these files with `view_file` (if they exist):
>    - `architecture.md` — project-specific design, toolchain, and patterns
>    - `context.md` — historical decisions and prior context
>    - `.agents/rules/coding-standard.md` — governance core rules. Next, check its Language Dispatch Table to determine which language skill files from `.gemini/skills/` to read based on the task's language.
>    - `.agents/rules/ipr.md` — implementation plan format and handoff rules
>    - `global/skills/codebase-recon/SKILL.md` — skill to spawn the recon subagent
>    - `global/skills/plan-reviewer/SKILL.md` — skill to spawn the plan reviewer subagent
>    - `global/skills/library-researcher/SKILL.md` — (Optional) skill for external library research
>    - `global/skills/history-researcher/SKILL.md` — (Optional) skill for codebase evolution and historical context research
>    - `global/skills/api-planner/SKILL.md` — (Optional) skill for API design & interface contracts
>    - `global/skills/type-planner/SKILL.md` — (Optional) skill for type hierarchy & domain modeling
>    - `global/skills/module-planner/SKILL.md` — (Optional) skill for module topology & file hierarchy
>    - `global/skills/test-planner/SKILL.md` — (Optional) skill for test strategy & TDD scaffolding
>    - `global/skills/concurrency-planner/SKILL.md` — (Optional) skill for async boundaries & concurrency
>    - `global/skills/security-planner/SKILL.md` — (Optional) skill for security architecture & defensive constraints
>    - `global/skills/perf-planner/SKILL.md` — (Optional) skill for performance constraints & optimization
> 2. Run these auto-runnable commands:
// turbo
>    - `git log -n 20 --oneline`
// turbo
>    - `make search-todos`

- If the plan scope requires multiple phases (assessed during Phase 0), also read `.agents/rules/phase-rules.md` for phase manifest format, STUB conventions, and phase gate requirements.
- Confirm you are operating in **Planning mode** (no code edits allowed).

## Phases

> [!NOTE]
> **Graceful Fallback**: When `invoke_subagent` is unavailable (single-agent mode), the Architect executes Phases 1, 3, and 5 inline — performing reconnaissance, specialist domain planning, and plan review tasks directly rather than delegating to subagents. The phase structure remains the same; only the executor changes.

### 0. Scope Triage

- If a Report was produced by `/issue`, `/audit`, `/review`, or `/feature` **and was explicitly cited** by the user or invoking workflow, read it with `view_file`. Then perform **Report Distillation**:
  1. Extract all `File:Line:Function Signature` (or entity sentinel) targets from the report's affected files, findings matrix, or component list.
  2. Extract the core problem/intent summary, adapting by report type: root cause + severity (for `/issue`); health assessment + findings summary (for `/review`); fidelity gaps (for `/audit`); scope/requirements (for `/feature`).
  3. Record as a `Distilled Context` block, partitioned into domain-specific sub-blocks:
     - `Distilled Recon Context`: all seed `File:Line:Function Signature` (or entity sentinel) targets for `codebase-recon`.
     - `Distilled Historical Context`: seed targets for `history-researcher`.
     - Problem/intent summaries retained for Phase 2 Architect structured reasoning.
- Identify the rough tier (S-tier for 1-3 trivial files; M/L-tier for complex changes).
- Determine which subagents to spawn in Phase 1:
  - **codebase-recon**: Spawn for all M/L-tier plans. (Skip for trivial S-tier).
  - **rust-idiom-researcher**: Spawn if the target is Rust and scope touches >2 source files.
  - **library-researcher**: Spawn if the scope involves new dependencies, unfamiliar libraries, or complex algorithms/math not well understood.
  - **history-researcher**: Spawn if the scope touches files with significant change history (>10 commits), or if `context.md` / ADRs reference prior decisions about the affected area.

### 1. Subagent Reconnaissance *(M/L tier — conditional)*

Spawn the necessary subagents **in parallel**. Follow the respective `.gemini/skills/` instructions for each subagent. Adhere strictly to the prompt contract for every invocation:
- Include `Repository Workspace: <path>` (explicit workspace root path).
- Include boundary reminder: `"Confine all searches strictly to the repository workspace; never search parent or user directories."`
- Include report formatting reminder: `"Format findings strictly following the report template provided in your system prompt."`

Subagent targets:
- **codebase-recon**: Pass the affected scope (crate/module path) and — if a `Distilled Recon Context` was produced — all its `File:Line:Function Signature` (or entity sentinel) entries as blast-radius seed symbols.
- **rust-idiom-researcher** (optional): Pass the affected modules.
- **library-researcher** (optional): Pass the specific libraries/concepts to research.
- **history-researcher** (optional): Pass the specific `File:Line:Function Signature` (or entity sentinel) entries from the `Distilled Historical Context` block as investigation seed symbols, plus any ADR references.

**Wait:** Stop calling tools and wait for all spawned subagents to return their structured reports.

### 2. Architect Synthesis

Read the Reconnaissance Report (and Idiom/Library/History Reports if spawned).

- Determine the final plan tier (S/M/L) based on the reports' affected files count and complexity.
- Identify key architectural decisions, risks, and reuse opportunities from the reports.
- **Structured Reasoning (M/L tier — mandatory)**: Use `sequentialthinking` with 3–5 thoughts to reason through:
  1. **Root cause validation** — Is the proposed change solving the right problem?
  2. **Change ordering** — What's the dependency graph of the changes? What must come first?
  3. **Blast radius analysis** — Using the Recon report, map structural consumers and downstream impact. Populate the Blast Radius Table (see `ipr.md`).
  4. **Interface contract risks** — Will any signature changes break downstream callers?
  5. **Confidence check** — After the above, does the plan still make sense or does scope need adjustment?
  6. **Sub-Planner Distillation** *(M/L tier — if multi-agent available)*:
     Based on the synthesis above, determine which sub-planner domains are relevant to this plan.
     For each activated domain, compose a focused, scoped prompt containing ONLY what that specialist needs:
     - **api-planner**: Specific public symbols being added/changed, error domain, backward-compat constraints.
     - **type-planner**: New domain concepts to model, existing related types, migration needs.
     - **module-planner**: Affected module scope, cross-module dependencies, new files to introduce.
     - **test-planner**: Functions to test (name + signature), external dependencies to mock, integration boundaries.
     - **concurrency-planner**: Async boundaries, concurrency primitives, shared-state requirements, task topology.
     - **security-planner**: Trust boundaries, auth model, input surfaces, upstream security findings.
     - **perf-planner**: Hot paths, algorithmic choices, caching opportunities, upstream performance findings.

     Record these as a `Distilled Sub-Planner Context` block in memory before proceeding to Phase 3.

> [!CAUTION]
> If blast radius analysis reveals cross-package impact, the **Deprecation Protocol** defined in `ipr.md` applies. The plan MUST include a Deprecation Schedule section.

### 3. Domain Specialist Planning *(M/L tier — conditional)*

> [!NOTE]
> **Graceful Fallback**: When `invoke_subagent` is unavailable (single-agent mode),
> skip this phase. The Architect covers all design domains inline during Phase 2
> Structured Reasoning and Phase 4 drafting.

**Gate:** Skip for S-tier plans. For M/L tier, activate sub-planners based on Phase 2 assessment.

**Activation is intent-based, not tier-based.** The Architect decides which domains are relevant during Phase 2's Distillation step. Only activate a sub-planner when the plan involves that domain's concern:

| Sub-Planner | Skill | Activate When |
|---|---|---|
| `api-planner` | `global/skills/api-planner/SKILL.md` | Plan adds or modifies public signatures, endpoints, or trait definitions |
| `type-planner` | `global/skills/type-planner/SKILL.md` | Plan adds new types, modifies type definitions, or introduces new domain concepts |
| `module-planner` | `global/skills/module-planner/SKILL.md` | Affected files touch across different domains or modules; cross-module deps detected |
| `test-planner` | `global/skills/test-planner/SKILL.md` | Plan adds or modifies functions that require test coverage |
| `concurrency-planner` | `global/skills/concurrency-planner/SKILL.md` | Plan involves async/await, task spawning, channels, mutexes, or parallel pipelines |
| `security-planner` | `global/skills/security-planner/SKILL.md` | Plan involves auth, input validation, crypto, trust boundaries, or upstream security findings |
| `perf-planner` | `global/skills/perf-planner/SKILL.md` | Plan involves hot paths, algorithm choices, caching, or upstream performance findings |

For each activated domain:
1. Load its skill using `view_file`.
2. Call `define_subagent` per § 1 of the skill.
3. Call `invoke_subagent` using the **Distilled Sub-Planner Context** prompt prepared in Phase 2 — NOT the raw recon report.
   - Include `Repository Workspace: <path>` (explicit workspace root path).
   - Include boundary reminder: `"Confine all searches strictly to the repository workspace; never search parent or user directories."`
   - Include: `"Format findings strictly following the report template provided in your system prompt."`

> 🤖 Announce each spawned subagent per `GEMINI.md §10`:
> `🤖 Spawning subagent [Role] with model: flash (Gemini 3.8 Flash High)`

**🛑 Turn Boundary Fence:** Invoking the final sub-planner MUST be the last tool call of this turn.
Stop calling tools. The system will deliver all specialist reports via reactive wakeup.

**Wait:** Do NOT proceed to Phase 4 until ALL spawned sub-planner reports are received.

### 4. Draft the Plan

> 🛑 **Conversational Chat Output Fence:**
> During Phases 4, 5, and 6, the implementation plan is held STRICTLY in memory. Do NOT dump or print the draft plan markdown, sections, or steps into the conversational chat response. Chat output during these phases is strictly restricted to subagent model declarations (per `GEMINI.md §10`) and concise one-line status updates. The complete plan is written to disk and linked to the user only in Phase 7.

> 📋 **Consolidate Specialist Fragments First:** Before drafting, incorporate the
> sub-planner reports directly into their target plan sections. Do NOT re-derive
> what specialists already produced. Override a fragment only if it contradicts
> `architecture.md` — and document the override reason inline.

> 📘 **Skill:** [`scaffold-plan`](../../.gemini/skills/scaffold-plan/SKILL.md) — load to extract the exact markdown template scaffolding.

Draft the implementation plan adhering strictly to the extracted template from `scaffold-plan`:
- **S-Tier Plans:** Write the plan directly to `<artifacts>/implementation_plan.md` using `write_to_file` (`IsArtifact: true`, `RequestFeedback: true`). Proceed directly to Phase 7.
- **M/L-Tier Plans (In-Memory Draft):** Prepare the complete implementation plan markdown text in memory. **Do NOT call `write_to_file` to write `implementation_plan.md` to disk yet.** Holding the plan in memory prevents premature artifact feedback notifications and ensures unreviewed plans are not exposed to the user or Builder before architectural review.

Incorporate the subagent reports' findings, Blast Radius Table, and recommended idioms directly into the drafted plan.

### 5. Subagent Plan Review *(M/L tier — conditional)*

> 🛑 **Conversational Chat Output Fence:**
> During Phases 4, 5, and 6, the implementation plan is held STRICTLY in memory. Do NOT dump or print the draft plan markdown, sections, or steps into the conversational chat response. Chat output during these phases is strictly restricted to subagent model declarations (per `GEMINI.md §10`) and concise one-line status updates. The complete plan is written to disk and linked to the user only in Phase 7.

**Gate:** Skip for S-tier plans (trivial scope, low blast radius). Always run for M/L tier.

**Path A: Multi-Agent Subagent Review (Standard)**
When running M/L-tier plans in multi-agent mode, submit the draft plan to the `plan-reviewer` subagent:
1. Load and follow the `plan-reviewer` skill (`global/skills/plan-reviewer/SKILL.md`).
2. Call `define_subagent` if `plan_reviewer` is not yet defined.
3. Call `invoke_subagent` adhering to the mandatory prompt contract:
   - Format the `Prompt` using the canonical schema from `plan-reviewer §2`:
     ```text
     Repository Workspace: <path>
     Target Scope: <crate/module path>
     Review Cycle: <N> of 3
     Review Priorities: <DRY, module cohesiveness, architectural harmony, API design>
     Revision Notes: <"Initial Draft" (Cycle 1) | "Summary of adjustments addressing prior reviewer findings" (Cycle 2+)>

     Negative Boundary Reminder:
     Confine all searches strictly to the repository workspace; never search parent or user directories.

     Report Formatting Reminder:
     Format findings strictly following the report template provided in your system prompt.

     Draft Plan Content:
     <complete draft markdown text>
     ```
   - Announce the model per `GEMINI.md §10` (`flash`).
4. 🛑 **Turn Boundary Fence:** Calling `invoke_subagent` **MUST be the final tool call of this turn**.
   - Do NOT call `write_to_file` for `implementation_plan.md` or `task.md`.
   - Do NOT run pre-flight checks.
   - Do NOT output "Think Phase Complete".
   - **Stop calling tools immediately** and yield the turn. The system will deliver the subagent's review report via reactive wakeup.

**Path B: Single-Agent Inline Review (Graceful Fallback)**
When `invoke_subagent` is unavailable, the Architect evaluates the in-memory draft directly against the 4 core design principles:
1. **DRY & Domain Duplication:** Grep workspace to ensure proposed types, helpers, and algorithms do not duplicate existing code.
2. **Module Cohesiveness:** Verify new files, structs, and functions are placed in appropriate domain boundaries.
3. **Architectural Harmony:** Verify adherence to `coding-standard.md`, tracing conventions, and error types.
4. **API Design Ergonomics:** Verify minimal public exposure, appropriate borrow semantics (`&str`), and complete error contracts.
Record the evaluation in the plan's `### Review History & Verdict` table (`Reviewer: inline`, `Verdict: ✅ Approved`). If issues are found, revise the draft before proceeding to Phase 7.

### 6. Architect Assessment & Plan Revision

> 🛑 **Conversational Chat Output Fence:**
> During Phases 4, 5, and 6, the implementation plan is held STRICTLY in memory. Do NOT dump or print the draft plan markdown, sections, or steps into the conversational chat response. Chat output during these phases is strictly restricted to subagent model declarations (per `GEMINI.md §10`) and concise one-line status updates. The complete plan is written to disk and linked to the user only in Phase 7.

Track the current cycle using the `Review Cycle: [N] of 3` metadata present in the subagent invocation prompt, the report header, and the plan's `### Review History & Verdict` table. Upon receiving the reactive wakeup containing the `# Implementation Plan Design Review Report: <Plan Title>`, evaluate the review verdict:

- **✅ Approved** (at any cycle) → Record verdict in `### Review History & Verdict`. Adopt the draft plan as final. Proceed to Phase 7.
- **⚠️ Revisions Recommended** AND **cycle < 3**:
  1. Revise the in-memory draft plan addressing the report's Required Plan Adjustments.
  2. Record adjustments in the plan's `### Review History & Verdict` table.
  3. Re-spawn `plan-reviewer` passing the full revised draft text with `Review Cycle: [N+1] of 3` and revision notes in the canonical prompt schema.
  4. Apply the Turn Boundary Fence (Phase 5, step 4) and await the next wakeup.
- **⚠️ Revisions Recommended** AND **cycle = 3** (cap exhausted):
  1. Record final cycle in `### Review History & Verdict`.
  2. Append a `## ⚠️ Reviewer Findings (Unresolved)` section to the bottom of the drafted plan, listing the Required Plan Adjustments from the final report for explicit user sign-off.
  3. Adopt this annotated draft as final. Proceed to Phase 7.
- **🛑 Major Rethink Required** (at any cycle) → Immediately stop the loop. Inform the user of the reviewer's structural concerns and recommend scope adjustment before proceeding.

### 7. Finalize Plan, Sync & Pre-Flight Gate

1. **Persist Implementation Plan:**
   - Write the finalized implementation plan to `<artifacts>/implementation_plan.md` using `write_to_file` (`IsArtifact: true`, `RequestFeedback: true`).
   - Include a clickable markdown link in your chat response (e.g., `[Implementation Plan](file:///absolute/path/to/implementation_plan.md)`).

2. **Generate `task.md` from the plan:**
   - Read the plan file with `view_file`.
   - Extract the `### Plan Objectives` table rows from the plan (ID, Objective, Success Criteria, Steps columns).
   - Extract all entries matching `Step N: [ACTION] filepath — [+|~|-] symbol (L##-##)`.
   - Write `task.md` to the same directory as the plan:

   ```markdown
   # Task: <plan title from first # heading>

   ## Plan Objectives
   | ID | Objective | Success Criteria | ✅ |
   |----|-----------|-----------------|----|
   | O1 | <from plan> | <from plan> | [ ] |
   | O2 | <from plan> | <from plan> | [ ] |

   ## Implementation Steps
   - [ ] <Component 1>
     - [ ] [ACTION] <filename>
   - [ ] <Component 2>
     - [ ] [ACTION] <filename>
   - [ ] Run verification pipeline
   - [ ] Update docs
   - [ ] Update context.md
   - [ ] Commit
   ```

Before requesting approval, verify these 8 critical fail-path checks:

1. **🤖 Planning Gate** — No code was edited during this workflow.
2. **🤖 File coverage** — All affected files are listed with `[NEW|MODIFY|DELETE|TEST]` tags.
3. **🧠 Deprecation Protocol** — If blast radius shows cross-package impact, the Deprecation Schedule section is present (`ipr.md`).
4. **🧠 TDD ordering** — Test cases are specified *before* implementation steps in the Global Execution Order.
5. **🤖 task.md alignment** — Run the **Validate task.md** procedure:
   > 📘 **Skill:** [`validate-task-alignment`](.gemini/skills/validate-task-alignment/SKILL.md) — run the Validate task.md procedure
   1. Read both `task.md` and the plan file with `view_file`.
   2. Check that every `[NEW|MODIFY|DELETE|TEST] filename` in the plan appears in `task.md`.
   3. Check that every such entry in `task.md` appears in the plan.
   4. If mismatches → report and STOP.
6. **🤖 Topological sort** — Global Execution Order is topologically sorted (Narrowing = bottom-up; Widening = top-down).
7. **🧠 Plan Objectives check** — Verify that `### Plan Objectives` table is present, contains at least one data row, and each row maps to `task.md`.
8. **🧠 Review Verdict** — Review verification passed under one of these valid terminal states:
   - **S-Tier:** N/A (review skipped per tier gate).
   - **Multi-Agent Approved:** `plan-reviewer` subagent returned `✅ Approved`.
   - **Single-Agent Approved:** Inline review checklist verified and passed.
   - **Cycle Cap Exhausted:** Reviewer returned `⚠️ Revisions Recommended` on Cycle 3, and all remaining items are explicitly documented in `## ⚠️ Reviewer Findings (Unresolved)` for user sign-off.

> [!CAUTION]
> All 8 checks MUST pass before requesting approval. If any fails, fix and re-check.

> [!NOTE]
> **Post-Approval:** Once approved, follow **GEMINI.md §5 Handoff Protocol** for the full Act cycle. After `/audit` passes, run `/update-doc` scoped to affected files, then summarize in `context.md`.

End with:

> 🛑 **Think Phase Complete.** Reply with **"Proceed"** to Act.

Do NOT proceed to implementation until the user explicitly approves.

## Rules

1. **Planning Mode Gate:** No source code files may be created or edited during this workflow.
2. **Subagent Reconnaissance Mandate:** Never plan M/L-tier changes without running `codebase-recon` to map blast radius and dependencies.
3. **Intent-Based Sub-Planners:** Activate specialist sub-planners only when their specific technical domain is impacted.
4. **In-Memory Draft Discipline:** M/L-tier plans MUST remain in memory throughout the review cycle. Never write unapproved drafts to disk.
5. **Conversational Chat Output Fence:** Never output draft plan contents into the chat conversation. Chat output is restricted to model announcements and brief status updates.
6. **Leak-Proof Review Verification:** Pre-Flight Check 8 must verify one of the 4 valid terminal states. Single-turn bypass is strictly prohibited.
