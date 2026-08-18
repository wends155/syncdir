---
description: How to create a high-quality implementation plan (Think Phase)
---

# Plan-Making Workflow

This workflow defines the standard process for creating implementation plans.
It enforces the Planning Gate and Think Phase of the TARS protocol.

## Prerequisites

> [!IMPORTANT]
> **Execution Discipline:** You **MUST** use the `view_file` tool to read all listed rule files before starting Phase 1. Do not rely on internal memory.

> [!TIP]
> Load context using native agent tools (zero-prompt):
> 1. Read these files with `view_file` (if they exist):
>    - `architecture.md` — project-specific design, toolchain, and patterns
>    - `context.md` — historical decisions and prior context
>    - `.agents/rules/coding-standard.md` — governance core rules. Next, check its Language Dispatch Table to determine which language skill files from `.gemini/skills/` to read based on the task's language.
>    - `.agents/rules/ipr.md` — implementation plan format and handoff rules
> 2. Run these auto-runnable commands:
// turbo
>    - `git log -n 20 --oneline`
// turbo
>    - `make search-todos`

- If the plan scope requires multiple phases (assessed during Phase 1), also read `.agents/rules/phase-rules.md` for phase manifest format, STUB conventions, and phase gate requirements.
- If a Report was produced by `/issue`, `/audit`, or `/feature`, use it as the **primary input** for Phase 1. Do not re-investigate areas already covered.
- Confirm you are operating in **Planning mode** (no code edits allowed).

## Phases

### 1. Scope & Impact Analysis

Investigate the request before writing anything:

- **Identify affected files**: List every file/module that will be touched.
- **Map dependencies**: What depends on those files? What do they depend on?
- **Flag risks**: Security concerns, breaking changes, performance impacts.
- **Check for existing tests**: Search for test files related to the affected code.
- **Assess phase scope**: Determine if this is a single-phase or multi-phase plan. Multi-phase indicators: scope touches >3 modules, requires infrastructure setup before features, or depends on stubs from prior phases. If multi-phase, load `phase-rules.md` and include Phase Context / Phase Manifest in the plan.

#### MCP-Enhanced Analysis *(when available)*

When Narsil MCP is available, use it for blast radius analysis (`get_callers`, `get_callees`, `get_call_graph`), security scanning (`scan_security`, `check_cwe_top25`), interface understanding (`find_symbols`, `find_references`, `get_symbol_definition`), and cleanup opportunities (`find_unused_exports`, `find_dead_code`). Use `find_similar_code` to discover existing patterns the plan should follow.

When **Knowledge-RAG MCP** is available, use `search_knowledge` to check for pre-ingested dependency documentation, coding patterns, or prior TARS rule context relevant to the scope. This avoids redundant web lookups for dependencies already indexed locally.

#### Pre-Draft Review *(M/L tier)*

Before drafting, apply selected `/review` lenses on the **existing code** being targeted for change:

- **🏗️ Design** (always for M/L tier) — catches coupling risks → document in Edge Cases & Risks.
- **📐 API** (when modifying public interfaces) — catches breaking changes → trigger Deprecation Protocol (`ipr.md`).
- **🔒 Security** (when modifying taint-carrying code) — catches taint issues → mandate sanitization.

> [!NOTE]
> **Logic** and **Performance** lenses are skipped at plan time — those are post-implementation concerns handled by `/audit` and `/review`.

### 2. Reasoning & Draft

#### Structured Reasoning *(M/L tier — mandatory; S-tier — skip)*

Before drafting the plan, use `sequentialthinking` with 3–5 thoughts to reason through:

1. **Root cause validation** — Is the proposed change solving the right problem?
2. **Change ordering** — What's the dependency graph of the changes? What must come first?
3. **Blast radius analysis** — Using Narsil `get_callers`/`get_callees`/`get_call_graph` (or fallback to `find_references`/`rg`), map structural consumers and downstream impact. Populate the Blast Radius Table (see `ipr.md`).
4. **Interface contract risks** — Will any signature changes break downstream callers?
5. **Confidence check** — After the above, does the plan still make sense or does scope need adjustment?

> [!CAUTION]
> If blast radius analysis reveals cross-package impact, the **Deprecation Protocol** defined in `ipr.md` applies. The plan MUST include a Deprecation Schedule section.

#### Draft the Plan

> 📘 **Skill:** [`scaffold-plan`](../../.gemini/skills/scaffold-plan/SKILL.md) — load to extract the exact markdown template scaffolding.

Write the implementation plan to `<artifacts>/implementation_plan.md` using the `write_to_file` tool (`IsArtifact: true`). You **MUST** explicitly load `.gemini/skills/scaffold-plan/SKILL.md` and carefully rip/extract the literal `<!-- TEMPLATE_START: ... -->` markdown scaffolding block corresponding to the tier. Do **NOT** try to parse or deduce the plan structure from declarative instructions in `ipr.md` or from system-default plan templates (see `GEMINI.md §8`).

> [!NOTE]
> Once the artifact is written, you **MUST** provide a clickable markdown link to it in your final chat response (e.g., `[Implementation Plan](file:///absolute/path/to/implementation_plan.md)`).

### 3. Sync task.md (Agent Procedure)

Generate `task.md` from the plan:

1. Read the plan file with `view_file`.
2. Extract all headings matching `### ComponentName` and `#### [NEW|MODIFY|DELETE|TEST] filename`.
3. Write `task.md` to the same directory as the plan:

   ```markdown
   # Task: <plan title from first # heading>

   ## Objectives
   - [ ] <Component 1>
     - [ ] [ACTION] <filename>
   - [ ] <Component 2>
     - [ ] [ACTION] <filename>
   - [ ] Run verification pipeline
   - [ ] Update docs
   - [ ] Update context.md
   - [ ] Commit
   ```

> [!WARNING]
> task.md must be aligned with the plan before requesting approval.

### 4. Pre-Flight Gate

Before requesting approval, verify these 6 critical fail-path checks:

1. **🤖 Planning Gate** — No code was edited during this workflow.
2. **🤖 File coverage** — All affected files are listed with `[NEW|MODIFY|DELETE|TEST]` tags. Verify with Narsil `get_callers`/`get_callees` if available.
3. **🧠 Deprecation Protocol** — If blast radius shows cross-package impact, the Deprecation Schedule section is present (`ipr.md`).
4. **🧠 TDD ordering** — Test cases are specified *before* implementation steps in the Global Execution Order.
5. **🤖 task.md alignment** — Run the **Validate task.md** procedure:
   > 📘 **Skill:** [`validate-task-alignment`](.gemini/skills/validate-task-alignment/SKILL.md) — run the Validate task.md procedure
   1. Read both `task.md` and the plan file with `view_file`.
   2. Check that every `[NEW|MODIFY|DELETE|TEST] filename` in the plan appears in `task.md`.
   3. Check that every such entry in `task.md` appears in the plan.
   4. If mismatches → report and STOP.
6. **🤖 Topological sort** — Global Execution Order is topologically sorted (Narrowing = bottom-up; Widening = top-down).

> [!CAUTION]
> All 6 checks MUST pass before requesting approval. If any fails, fix and re-check.

> [!NOTE]
> **Post-Approval:** Once approved, follow **GEMINI.md §6 Handoff Protocol** for the full Act cycle. After `/audit` passes, run `/update-doc` scoped to affected files, then summarize in `context.md`.

End with:

> 🛑 **Think Phase Complete.** Reply with **"Proceed"** to Act.

Do NOT proceed to implementation until the user explicitly approves.


## Rules



