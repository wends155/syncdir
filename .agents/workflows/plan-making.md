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
> 2. Run these auto-runnable commands:
// turbo
>    - `git log -n 20 --oneline`
// turbo
>    - `make search-todos`

- If the plan scope requires multiple phases (assessed during Phase 0), also read `.agents/rules/phase-rules.md` for phase manifest format, STUB conventions, and phase gate requirements.
- Confirm you are operating in **Planning mode** (no code edits allowed).

## Phases

> [!NOTE]
> **Graceful Fallback**: When `invoke_subagent` is unavailable (single-agent mode), the Architect executes Phases 1 and 4 inline — performing the reconnaissance and plan review tasks directly rather than delegating to subagents. The phase structure remains the same; only the executor changes.

### 0. Scope Triage

Investigate the request before writing anything:
- If a Report was produced by `/issue`, `/audit`, or `/feature`, read it briefly for scope triage.
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
- **codebase-recon**: Pass the affected scope (crate/module path, list of files/functions) and any relevant context from a prior `/issue` report.
- **rust-idiom-researcher** (optional): Pass the affected modules.
- **library-researcher** (optional): Pass the specific libraries/concepts to research.
- **history-researcher** (optional): Pass the affected files/modules and any known ADR references.

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

> [!CAUTION]
> If blast radius analysis reveals cross-package impact, the **Deprecation Protocol** defined in `ipr.md` applies. The plan MUST include a Deprecation Schedule section.

### 3. Draft the Plan

> 📘 **Skill:** [`scaffold-plan`](../../.gemini/skills/scaffold-plan/SKILL.md) — load to extract the exact markdown template scaffolding.

Draft the implementation plan adhering strictly to the extracted template from `scaffold-plan`:
- **S-Tier Plans:** Write the plan directly to `<artifacts>/implementation_plan.md` using `write_to_file` (`IsArtifact: true`, `RequestFeedback: true`). Proceed directly to Phase 6.
- **M/L-Tier Plans (In-Memory Draft):** Prepare the complete implementation plan markdown text in memory. **Do NOT call `write_to_file` to write `implementation_plan.md` to disk yet.** Holding the plan in memory prevents premature artifact feedback notifications and ensures unreviewed plans are not exposed to the user or Builder before architectural review.

Incorporate the subagent reports' findings, Blast Radius Table, and recommended idioms directly into the drafted plan.

### 4. Subagent Plan Review *(M/L tier — conditional)*

**Gate:** Skip for S-tier plans (trivial scope, low blast radius). Always run for M/L tier.

When running M/L-tier plans, submit the draft plan to the `plan-reviewer` subagent:
1. Load and follow the `plan-reviewer` skill (`global/skills/plan-reviewer/SKILL.md`).
2. Call `define_subagent` if `plan_reviewer` is not yet defined.
3. Call `invoke_subagent` adhering to the mandatory prompt contract:
   - Pass the **complete draft plan markdown content** directly in the `Prompt` parameter.
   - Include the target crate/module scope and specific review priorities.
   - Include `Repository Workspace: <path>` (explicit workspace root path).
   - Include boundary reminder: `"Confine all searches strictly to the repository workspace; never search parent or user directories."`
   - Include report formatting reminder: `"Format findings strictly following the report template provided in your system prompt."`
   - Announce the model per `GEMINI.md §10` (`flash`).
4. 🛑 **Turn Boundary Fence:** Calling `invoke_subagent` **MUST be the final tool call of this turn**.
   - Do NOT call `write_to_file` for `implementation_plan.md` or `task.md`.
   - Do NOT run pre-flight checks.
   - Do NOT output "Think Phase Complete".
   - **Stop calling tools immediately** and yield the turn. The system will deliver the subagent's review report via reactive wakeup.

### 5. Architect Assessment & Plan Revision

Maintain a **Revision Cycle Counter** (mental state) starting at 1. Upon receiving the reactive wakeup containing the Design Review Report, evaluate the review verdict:

- **✅ Approved** (at any cycle) → Adopt the draft plan as final. Proceed to Phase 6.
- **⚠️ Revisions Recommended** AND **cycle < 3**:
  1. Revise the in-memory draft plan addressing the report's Required Plan Adjustments.
  2. Increment your Revision Cycle Counter.
  3. Re-spawn `plan-reviewer` passing the full revised draft text and prepending "Revision Cycle [N] of 3" to the `Prompt`.
  4. Apply the Turn Boundary Fence (Phase 4, step 4) and await the next wakeup.
- **⚠️ Revisions Recommended** AND **cycle = 3** (cap exhausted):
  1. Append a `## ⚠️ Reviewer Findings (Unresolved)` section to the bottom of the drafted plan, listing the Required Plan Adjustments from the final report.
  2. Adopt this annotated draft as final. Proceed to Phase 6.
- **🛑 Major Rethink Required** (at any cycle) → Immediately stop the loop. Inform the user of the reviewer's structural concerns and recommend scope adjustment before proceeding.

### 6. Finalize Plan, Sync & Pre-Flight Gate

1. **Persist Implementation Plan:**
   - Write the finalized implementation plan to `<artifacts>/implementation_plan.md` using `write_to_file` (`IsArtifact: true`, `RequestFeedback: true`).
   - Include a clickable markdown link in your chat response (e.g., `[Implementation Plan](file:///absolute/path/to/implementation_plan.md)`).

2. **Generate `task.md` from the plan:**
   - Read the plan file with `view_file`.
   - Extract the `### Plan Objectives` table rows from the plan (ID, Objective, Success Criteria, Steps columns).
   - Extract all headings matching `### ComponentName` and `#### [NEW|MODIFY|DELETE|TEST] filename`.
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
8. **🧠 Review Verdict** — The plan-reviewer subagent (Phase 4) returned ✅ Approved (or ⚠️ Revisions Recommended and revisions were applied).

> [!CAUTION]
> All 8 checks MUST pass before requesting approval. If any fails, fix and re-check.

> [!NOTE]
> **Post-Approval:** Once approved, follow **GEMINI.md §6 Handoff Protocol** for the full Act cycle. After `/audit` passes, run `/update-doc` scoped to affected files, then summarize in `context.md`.

End with:

> 🛑 **Think Phase Complete.** Reply with **"Proceed"** to Act.

Do NOT proceed to implementation until the user explicitly approves.

## Rules
