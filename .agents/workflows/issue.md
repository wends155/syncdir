---
description: Intake, investigate, and report an issue before planning (Pre-Think Phase)
---

# Issue Workflow

This workflow defines the standard process for receiving, investigating, and
documenting an issue **before** any planning or implementation begins.
It is the entry point of the TARS cycle — the step that comes before `/plan-making`.

> [!IMPORTANT]
> This workflow is **read-only** — no code edits, no plans, no implementation.
> The only output is a structured **Issue Report** artifact.

> [!NOTE]
> The Issue Report produced here is the **input artifact** for `/plan-making`.
> Focus on diagnosis, not solutions — proposed fixes belong in the planning phase.

## Trigger

User invokes: `/issue <description>`

## Prerequisites

> [!IMPORTANT]
> **Execution Discipline:** You **MUST** use the `view_file` tool to read all listed rule files (e.g., `.agents/rules/...`) before starting Step 1. Do not rely on internal memory.

- Read `.agents/rules/issue-rules.md` for classification criteria, report format, and investigation depth.
- Read `global/skills/issue-investigator/SKILL.md` — skill to spawn the diagnostic investigation subagent.
- Read `global/skills/codebase-recon/SKILL.md` — skill to spawn the structural reconnaissance subagent.
- Read `architecture.md` (if present) for project structure, components, and toolchain.
- Read `context.md` (if present) for historical decisions and known issues.
- Confirm you are operating as the **Architect** role.

## Steps

### 1. Parse & Classify

Extract the following from the user's description using the classification rubric in `issue-rules.md` §1:

| Field         | Action                                                       |
|---------------|--------------------------------------------------------------|
| **Type**      | Classify: `bug`, `feature`, `chore`, `docs`, or `question`  |
| **Component** | Identify the affected area (e.g., TUI, API, CLI, Database)  |
| **Severity**  | Estimate: `critical`, `high`, `medium`, `low`                |
| **Summary**   | One-line restatement of the issue                            |

If the description is too vague to classify, **ask clarifying questions immediately**
before proceeding to Step 2.

### 2. Load Context

Gather background information:

> [!TIP]
> Load context using native agent tools (zero-prompt):
> 1. Read `architecture.md` and `context.md` with `view_file` (if they exist).
> 2. Run these auto-runnable commands:
// turbo
>    - `git log -n 20 --oneline`
// turbo
>    - `make search-todos`

- **`architecture.md`**: Identify relevant modules, patterns, and frameworks.
- **`context.md`**: Check for prior decisions, known bugs, or related history.
- **`git log -n 20`**: Review recent commits for changes in the affected area.
- **Existing issues/TODOs**: Search for related `TODO`, `FIXME`, `HACK` comments in the codebase.

### 2.5 Scope Triage

Before investigating, assess the investigation approach:
- **medium/high/critical severity** → Spawn subagents in parallel (Step 3).
- **low severity** → Architect investigates inline (lightweight — affected files only).
- Determine which subagents to spawn:
  - **issue-investigator**: Spawn for all medium/high/critical issues. Handles diagnostic research, root cause analysis, test coverage, and issue localization.
  - **codebase-recon**: Spawn for all medium/high/critical issues. Handles blast radius analysis, call graph mapping (callers/callees), and dependency impact.

> [!NOTE]
> **Graceful Fallback**: When `invoke_subagent` is unavailable (single-agent mode),
> the Architect executes Step 3 inline — performing the investigation directly.
> The step structure remains the same; only the executor changes.

### 3. Investigate

#### Subagent-Orchestrated Investigation *(medium/high/critical — default)*

Spawn both subagents **in parallel**. Follow the respective skill instructions for each:

**A. Issue Investigator** (diagnostic research):
> 📘 **Skill:** [`issue-investigator`](../../global/skills/issue-investigator/SKILL.md) — spawn the diagnostic investigation subagent

1. Load and follow the `issue-investigator` skill.
2. Call `define_subagent` if `issue_investigator` is not yet defined.
3. Call `invoke_subagent` adhering to the mandatory prompt contract:
   - Pass the **severity level**, **affected component**, **keywords** from the issue description, and any context gathered in Step 2.
   - Include `Repository Workspace: <path>` (explicit workspace root path).
   - Include boundary reminder: `"Confine all searches strictly to the repository workspace; never search parent or user directories."`
   - Include report formatting reminder: `"Format findings strictly following the report template provided in your system prompt."`
   - Announce the model per `GEMINI.md §10` (`flash`).

**B. Codebase Recon** (blast radius & call graph):
> 📘 **Skill:** [`codebase-recon`](../../global/skills/codebase-recon/SKILL.md) — spawn the structural reconnaissance subagent

1. Load and follow the `codebase-recon` skill.
2. Call `define_subagent` if `codebase_recon` is not yet defined.
3. Call `invoke_subagent` adhering to the mandatory prompt contract:
   - Pass the **affected component/area** and instruct it to focus on blast radius mapping, `get_callers`/`get_callees`/`get_call_graph` around the issue area.
   - Include `Repository Workspace: <path>` (explicit workspace root path).
   - Include boundary reminder: `"Confine all searches strictly to the repository workspace; never search parent or user directories."`
   - Include report formatting reminder: `"Format findings strictly following the report template provided in your system prompt."`
   - Announce the model per `GEMINI.md §10` (`flash`).

Both subagents are spawned in the **same `invoke_subagent` call block** (parallel).

4. **Stop calling tools** and wait for both subagents to return their reports.

#### Inline Investigation *(low severity or single-agent fallback)*

When investigating inline, search the codebase to understand the problem area:

- **Identify affected files**: `grep` / `ripgrep` for keywords related to the issue.
- **Read relevant code**: Outline the affected functions/modules.
- **Map dependencies**: What calls into or depends on the affected code?
- **Query knowledge-rag**: Search for dependency API docs or patterns related to the issue (e.g., `search_knowledge "crate:notify event debounce"`). Use indexed context before resorting to web search.
- **Look for obvious causes**: Missing error handling, logic errors, race conditions, etc.
- **Check tests**: Are there existing tests covering this area? Are they passing?

Scale investigation depth per `issue-rules.md` §3.

### 3.5 Architect Diagnostic Synthesis

Read **both** reports returned by the subagents:
- **Issue Investigation Report** → Root cause analysis, issue location (module/file/function), test coverage, related history.
- **Codebase Reconnaissance Report** → Blast Radius Table, dependency map (callers/callees), complexity hotspots, reuse opportunities.

Synthesize the two reports:
- Cross-reference the investigation's root cause against the recon's blast radius — does the root cause explain the downstream impact?
- Merge the issue location (from investigation) with the call graph context (from recon) to build the complete picture.
- For **critical/high** severity issues, use `sequentialthinking` to:
  - Structure complex, multi-factor investigations step by step.
  - Evaluate and discard competing hypotheses before settling on a root cause.
  - Assess whether the investigation report's root cause analysis is convincing given the blast radius data.
- For **medium** severity, validate the reports' findings with brief spot-checks if anything seems off.
- Flag any gaps or contradictions between the two reports.

> [!TIP]
> Keep synthesis focused. The goal is to validate the subagents' findings and
> add architectural judgment — not to re-investigate everything.

### 4. Produce Issue Report

> 📘 **Skill:** [`scaffold-issue-report`](../../.gemini/skills/scaffold-issue-report/SKILL.md) — generate the issue report skeleton from `issue-rules.md` format

Write the structured report to `<artifacts>/issue_report.md` using the `write_to_file` tool (`IsArtifact: true`). Follow the format in `issue-rules.md` §2. Include a clickable `[issue_report.md](file:///path)` artifact link in your chat response.

> [!CAUTION]
> Do **not** include proposed solutions, fixes, or implementation suggestions.
> See `issue-rules.md` §4 for full diagnostic constraints.

> [!NOTE]
> Once the artifact is written, you **MUST** provide a clickable markdown link to it in your final chat response (e.g., `[Issue Report](file:///absolute/path/to/issue_report.md)`).

### 5. Pause for Refinement

End the report with:

> 🛑 **Issue Analysis Complete.**
> Please review the findings above. You can:
> - **Clarify** or **refine** the issue description
> - **Adjust** severity or component classification
> - **Add** additional context or constraints
>
> When satisfied, reply with **"Plan"** to proceed to `/plan-making`.

**Do NOT proceed to planning until the user explicitly approves the issue report.**

## Rules

1. **No code edits** — this is an investigation-only workflow.
2. **No planning** — do not propose solutions or implementation steps.
3. **Always pause** — the user must explicitly say "Plan" to move forward.
4. **Ask early** — if the issue is ambiguous, ask questions in Step 1, not Step 4.
5. **Stay focused** — investigate just enough to produce a clear report; avoid rabbit holes.
6. **Use subagents** — when `invoke_subagent` is available, delegate investigation to `issue-investigator` and `codebase-recon` subagents in parallel for medium/high/critical issues. When Narsil or Sequential Thinking MCP are available in single-agent fallback, prefer them over manual grep/search.


