---
description: Research, evaluate feasibility, and report on a new feature request (Pre-Think Phase)
---

# Feature Workflow

This workflow defines the standard process for receiving a feature request,
researching its feasibility, evaluating alternatives, and producing a structured
report **before** any planning or implementation begins.
Like `/issue`, this is a pre-planning step that feeds into `/plan-making`.

> [!IMPORTANT]
> This workflow is **read-only** — no code edits, no plans, no implementation.
> The only output is a structured **Feature Research Report** artifact.

## Trigger

User invokes: `/feature <description>`

## Prerequisites

> [!IMPORTANT]
> **Execution Discipline:** You **MUST** use the `view_file` tool to read all listed rule files (e.g., `.agents/rules/...`) before starting Step 1. Do not rely on internal memory.

> [!TIP]
> Load context using native agent tools (zero-prompt):
> 1. Read `architecture.md` and `context.md` with `view_file` (if they exist).
> 2. Run these auto-runnable commands:
// turbo
>    - `git log -n 20 --oneline`
// turbo
>    - `make search-todos`

- Read `.agents/rules/feature-rules.md` for classification criteria, report format, and architectural fit assessment.
- Read `architecture.md` (if present) for project structure, patterns, and constraints.
- Read `.agents/rules/coding-standard.md` (if present) for governance core rules. Next, check its Language Dispatch Table to determine which language skill files from `.gemini/skills/` to read based on the task's language.
- Read `context.md` (if present) for historical decisions and prior feature work.
- Confirm you are operating as the **Architect** role.

## Steps

### 1. Parse & Understand

Extract the following from the user's description using the classification rubric in `feature-rules.md` §1:

| Field            | Action                                                              |
|------------------|---------------------------------------------------------------------|
| **Feature Name** | Short, descriptive name for the feature                             |
| **Category**     | Classify: `enhancement`, `new-capability`, `integration`, `refactor`|
| **Component**    | Identify the affected area (e.g., TUI, API, CLI, Core Library)      |
| **Priority**     | Estimate: `must-have`, `should-have`, `nice-to-have`                |
| **Summary**      | One-line restatement of the desired outcome                         |

If the description is too vague to understand the user's intent,
**ask clarifying questions immediately** before proceeding to Step 2.

### 2. Load Context

Gather background information (or use the output from `Load-Context.ps1` above):

- **`architecture.md`**: Identify relevant modules, patterns, frameworks, and constraints.
- **`context.md`**: Check for related prior discussions, rejected ideas, or relevant decisions.
- **`git log -n 20`**: Review recent work for anything related to this feature area.
- **Existing code**: Search for any partial implementations, stubs, or `TODO`s related to the feature.

### 3. Research & Evaluate

Investigate how the feature could be built:

#### 3a. Ecosystem Research
- **Search for existing libraries/crates/packages** that could fulfill the requirement.
- **Check knowledge-rag** for pre-indexed dependency docs (`search_knowledge` with `"crate:{name}"` or `"package:{name}"`). Use retrieved context before falling back to Context7 or web search.
- **Read documentation** for candidate dependencies (use Context7, docs, or web search).
- **Compare options**: license, maintenance status, compatibility, API ergonomics.

#### 3b. Feasibility Assessment
- **Architectural fit**: Does this align with the current architecture, or does it require changes?
- **Complexity estimate**: Rough sizing — `small` (hours), `medium` (1-2 days), `large` (3+ days).
- **Risk factors**: Breaking changes, performance concerns, new dependency risks.
- **Constraints**: Environment limitations (e.g., no admin rights), compatibility requirements.

#### 3c. Alternatives Analysis
- Identify **at least 2 approaches** where possible (including the user's original idea).
- For each approach, note: pros, cons, complexity, and trade-offs.
- **Recommend** a preferred approach with reasoning.

> [!TIP]
> The goal is to give the user enough information to make an informed decision —
> not to design the full solution (that's for `/plan-making`).

#### MCP-Enhanced Research *(when available)*

If **Narsil MCP** is available, use it to improve research accuracy:

| Tool | Purpose |
|------|--------|
| `search_code` / `semantic_search` | Find existing partial implementations or related code |
| `find_symbols` | Discover public API surface relevant to the feature |
| `get_import_graph` | Understand where the feature would integrate |
| `get_project_structure` | Understand module boundaries and layout |
| `find_similar_code` | Find patterns similar to what the feature needs |
| `get_dependencies` | Check existing dependency chain for integration points |

For **medium/large** features, the Architect **SHOULD** use `sequentialthinking` to:
- Structure the feasibility assessment systematically.
- Compare alternatives with consistent criteria.
- Evaluate architectural fit against `architecture.md` patterns.
- Identify non-obvious risks or constraints.

For **small** features, skip sequential thinking — the overhead isn't worth it.

### 4. Produce Feature Research Report

> 📘 **Skill:** [`scaffold-feature-report`](../../.gemini/skills/scaffold-feature-report/SKILL.md) — generate the feature report skeleton from `feature-rules.md` format

Create a structured report following the format in `feature-rules.md` §2.
Include the architectural fit assessment per `feature-rules.md` §3 (if `architecture.md` exists).

> [!NOTE]
> Once the artifact is written, you **MUST** provide a clickable markdown link to it in your final chat response (e.g., `[Feature Research Report](file:///absolute/path/to/feature_research_report.md)`).

### 5. Pause for Refinement

End the report with:

> 🛑 **Feature Research Complete.**
> Please review the findings above. You can:
> - **Clarify** or **refine** the feature description
> - **Choose** between the proposed approaches
> - **Adjust** priority or complexity assessment
> - **Add** constraints or requirements not yet captured
>
> When satisfied, reply with **"Plan"** to proceed to `/plan-making`.

**Do NOT proceed to planning until the user explicitly approves the report.**

## Rules

1. **No code edits** — this is a research-only workflow.
2. **No planning** — do not produce implementation steps or blueprints.
3. **Always pause** — the user must explicitly say "Plan" to move forward.
4. **Always suggest alternatives** — even if the user's idea is good, show options.
5. **Ask early** — if the feature is ambiguous, ask questions in Step 1, not Step 4.
6. **Leverage the ecosystem** — check for existing libraries before proposing custom code.
7. **Stay focused** — research just enough to inform a decision; avoid deep prototyping.


