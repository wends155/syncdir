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

## Workflow Persona

When executing this workflow, adopt the mindset of a **Lead Diagnostic Architect** coordinating
an issue intake and diagnostic pipeline. Your role is to parse the issue, triage the analytical
scope, orchestrate specialized diagnostic subagents for standard and deep investigations, and
synthesize findings into an authoritative diagnostic report. You delegate codebase investigation
for `medium`, `high`, and `critical` issues to specialized subagents (`issue-investigator` and
`codebase-recon`) running on efficient models. For `low` severity issues or in single-agent
fallback mode, you investigate inline using the lightweight depth defined in `issue-rules.md §3`.
You do NOT perform manual codebase excavation during medium/high/critical investigations —
scope triage and synthesis are your exclusive roles in those paths. You focus your reasoning
on scope triage, diagnostic synthesis, and severity calibration.

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

Extract the following from the user's description per the Classification Rubric in
`issue-rules.md §1`:

| Field | Value |
|-------|-------|
| **Type** | `bug` / `feature` / `chore` / `docs` / `question` (per `issue-rules.md §1`) |
| **Component** | Affected module, workflow, or system area |
| **Severity** | `critical` / `high` / `medium` / `low` (apply rubric in `issue-rules.md §1`) |
| **Summary** | One-line restatement of the issue |

If Type is `question` or `docs`, default Severity to `low`.
If the description is too vague to classify, **ask clarifying questions immediately** before
proceeding.

### 2. Scope Triage *(Zero-Tool Decision Gate)*

> [!IMPORTANT]
> **This is a zero-tool decision gate.** Do NOT run any tools, file reads, or code searches
> during this step. Route based solely on the severity classification from Step 1.

| Severity | Path |
|----------|------|
| `critical` / `high` / `medium` | **Subagent dispatch** — spawn BOTH subagents in parallel in Step 3. |
| `low` | **Inline investigation** — load targeted context in Step 2.5, then investigate inline in Step 3. |

> [!NOTE]
> **Graceful Fallback:** When `invoke_subagent` is unavailable (single-agent mode), the Architect
> executes Step 3 inline for all severities, following `issue-rules.md §3` depth guidance.

### 2.5 Targeted Context Load *(Low Severity & Single-Agent Fallback Only)*

Skip this step entirely if subagent dispatch was selected in Step 2.

Load minimal targeted context before beginning inline investigation:
1. Read `architecture.md` and `context.md` with `view_file` (if present).
2. Run `git log -n 10 --oneline` to surface recent changes in the affected area.

Do NOT run global marker searches or any global codebase search at this stage.
Module-scoped marker searches occur during Step 3 inline investigation.

### 3. Investigate

#### Subagent-Orchestrated Investigation *(medium/high/critical — default)*

Spawn both subagents **in parallel**. Follow the respective skill instructions for each:

**A. Issue Investigator** (diagnostic research):
> 📘 **Skill:** [`issue-investigator`](../../global/skills/issue-investigator/SKILL.md) — spawn the diagnostic investigation subagent

1. Load and follow the `issue-investigator` skill.
2. Call `define_subagent` if `issue_investigator` is not yet defined.
3. Call `invoke_subagent` adhering to the mandatory prompt contract:
   - Pass the **severity level**, **affected component**, **keywords** from the issue description,
     and any context gathered from the issue description and **Step 1 (Parse & Classify)**.
     If any `File:Line:Function Signature` coordinates are known from the user's description or
     a prior report, list each as: `<file path>:<line>:<Type::method_name()>` (or entity sentinel
     such as `struct <Name>`, `(Module)`, `(Config)`). Always include `File:Line:Function Signature`
     for every **known** target.
   - **Unknown Targets:** If no file coordinates are known (unlocalized symptom), explicitly specify:
     `Target: (Unknown — diagnose from symptom and component)`. Do NOT search the codebase to
     locate coordinates before dispatching; discovery is the subagent's responsibility.
   - Include `Repository Workspace: <path>` (explicit workspace root path).
   - Include boundary reminder: `"Confine all searches strictly to the repository workspace; never search parent or user directories."`
   - Include report formatting reminder: `"Format findings strictly following the report template provided in your system prompt."`
   - Announce the model per `GEMINI.md §10` (`flash`).

**B. Codebase Recon** (blast radius & call graph):
> 📘 **Skill:** [`codebase-recon`](../../global/skills/codebase-recon/SKILL.md) — spawn the structural reconnaissance subagent

1. Load and follow the `codebase-recon` skill.
2. Call `define_subagent` if `codebase_recon` is not yet defined.
3. Call `invoke_subagent` adhering to the mandatory prompt contract:
   - Pass the **affected component/area** and instruct it to focus on blast radius mapping,
     `get_callers`/`get_callees`/`get_call_graph` around the issue area.
     If any `File:Line:Function Signature` coordinates are known from the user's description or
     a prior report, list each as: `<file path>:<line>:<Type::method_name()>` (or entity sentinel
     such as `struct <Name>`, `(Module)`, `(Config)`). Always include `File:Line:Function Signature`
     for every **known** target.
   - **Unknown Targets:** If no file coordinates are known, explicitly specify:
     `Target: (Unknown — diagnose from symptom and component)`. Do NOT search before dispatching.
   - Include `Repository Workspace: <path>` (explicit workspace root path).
   - Include boundary reminder: `"Confine all searches strictly to the repository workspace; never search parent or user directories."`
   - Include report formatting reminder: `"Format findings strictly following the report template provided in your system prompt."`
   - Announce the model per `GEMINI.md §10` (`flash`).

Both subagents are spawned in the **same `invoke_subagent` call block** (parallel).

4. **Stop calling tools** and wait for both subagents to return their reports.

#### Inline Investigation *(Low Severity or Single-Agent Fallback)*

When investigating inline, use the following **token-efficient search hierarchy** (in priority order):

1. **Symbol / Code Discovery:** Use Narsil MCP (`search_code`, `find_symbols`,
   `get_symbol_definition`) if available.
2. **File Discovery:** Use `find_by_name` scoped to candidate subdirectories.
3. **Scoped Text Search:** Use `grep_search` restricted to candidate subtrees.
   Start with `MatchPerLine: false` (filename-only) before full-text matches.
   Cap results to 20 matches maximum.
4. **Targeted Reading:** Use `view_file` with explicit `StartLine`/`EndLine` slices.
   Do not read whole files.
5. **Domain Context:** Use Knowledge-RAG (`search_knowledge`) for domain/architectural
   context before web search.

Scale investigation depth per `issue-rules.md §3`. When complete, proceed directly to
**Step 4** — Step 3.5 is for subagent mode only (see bypass note there).

### 3.5 Architect Diagnostic Synthesis *(Subagent Mode Only)*

> [!NOTE]
> **Inline Bypass:** If investigation was conducted inline (low severity or single-agent fallback
> at Step 3), skip this step entirely and proceed directly to **Step 4**.
> Your inline findings are the synthesis input.

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

Adhere to **`issue-rules.md §4`** for all diagnostic constraints (no solutions or code edits,
no planning, ask early, token & search efficiency, bounded inspection).

Workflow-level rules:
1. **Always pause** — the user must explicitly reply with **"Plan"** before proceeding to
   `/plan-making`. Present the Issue Report and wait for approval.
2. **Use subagents** — for `medium`/`high`/`critical` severity issues, delegate investigation
   to `issue-investigator` and `codebase-recon` in parallel (Step 3). For `low` severity or
   single-agent fallback, investigate inline per `issue-rules.md §3`.
3. **Sequential Thinking gate** — in single-agent fallback, use `sequentialthinking` MCP only
   for `critical`/`high` severity. Skip for `medium`/`low` per `issue-rules.md §3`.


