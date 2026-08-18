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

### 3. Investigate

Search the codebase to understand the problem area:

- **Identify suspect files**: `grep` / `ripgrep` for keywords related to the issue.
- **Read relevant code**: Outline the affected functions/modules.
- **Map dependencies**: What calls into or depends on the affected code?
- **Query knowledge-rag**: Search for dependency API docs or patterns related to the issue (e.g., `search_knowledge "crate:notify event debounce"`). Use indexed context before resorting to web search.
- **Look for obvious causes**: Missing error handling, logic errors, race conditions, etc.
- **Check tests**: Are there existing tests covering this area? Are they passing?

#### MCP-Enhanced Investigation *(when available)*

If **Narsil MCP** is available, use it to improve investigation accuracy:
- **Code search & navigation**: `search_code`, `semantic_search`, `search_chunks` — find relevant code faster than manual grep.
- **Structural investigation**: `get_callers`, `get_callees`, `find_call_path` — trace the bug's propagation path and structural dependencies.
- **Complexity analysis**: `get_complexity`, `get_function_hotspots` — identify high-risk, brittle code areas.
- **Taint tracking**: `trace_taint`, `get_typed_taint_flow` — trace data flow from untrusted sources to sinks.
- **Security scanning**: `scan_security`, `check_owasp_top10`, `check_cwe_top25` — if the issue has security implications.

For **critical/high** severity issues, the Architect **SHOULD** use `sequentialthinking` to:
- Structure complex, multi-factor investigations step by step.
- Avoid jumping to conclusions by reasoning through causes systematically.
- Evaluate and discard competing hypotheses before settling on a root cause.

For **medium/low** severity, skip sequential thinking — the overhead isn't worth it.

Scale investigation depth per `issue-rules.md` §3.

> [!TIP]
> Keep investigation focused. The goal is to understand the problem well enough to
> write a clear report — not to find the exact fix (that's for `/plan-making`).

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
6. **Use MCP tools** — when Narsil or Sequential Thinking are available, prefer them over manual grep/search for higher accuracy.


