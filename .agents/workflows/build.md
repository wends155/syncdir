---
description: Formalized Act phase — execute an approved implementation plan (Act Phase)
---

# /build — Act Phase Workflow

Execute an approved implementation plan with full execution discipline,
scope fencing, and self-verification.

> [!IMPORTANT]
> This workflow is the **Builder's operating procedure**. It is loaded
> automatically when the user says "Proceed" after plan approval.

## Trigger

- `/build` — explicit invocation
- "Proceed" / "Implement" / "Execute" — after plan approval in `/plan-making`

## Prerequisites

> [!IMPORTANT]
> **Execution Discipline:** You **MUST** use the `view_file` tool to read all listed rule files (e.g., `.agents/rules/...`) before starting Step 1. Do not rely on internal memory.

> [!TIP]
> These are loaded in sequence. Do not skip any.

1. Read `.agents/rules/builder-rules.md` for execution discipline rules.
2. Read `.agents/rules/coding-standard.md` for governance core rules. Next, check its Language Dispatch Table to determine which language skill files from `.gemini/skills/` to read based on the task's language.
3. Load `.gemini/skills/knowledge-rag-query/SKILL.md` for the query-first dependency lookup convention. When the Builder encounters an unfamiliar API or pattern during implementation, query knowledge-rag before falling back to Context7 or web search.
4. Read the approved plan's **Builder Context** section — files and line ranges to read before starting.
5. Read the approved plan's **Negative Scope** section — files and areas NOT to touch.
6. Read `architecture.md` (if present) for project-specific design and toolchain commands.
7. If this is a multi-phase plan, read the prior phase's Phase Manifest (Deferred table in `context.md` or the prior plan). Verify that any `STUB(Phase N)` items where N = current phase are addressed as plan steps.
8. Confirm you are operating as the **Builder** (fast/efficient model).

## Steps

### 1. Pre-Flight

Verify the environment is ready:

> 📘 **Skill:** [`validate-task-alignment`](.gemini/skills/validate-task-alignment/SKILL.md) — cross-check task.md entries against plan

Invoke the skill to verify task.md is aligned with the plan.

**Capture test count baseline:**

Run the project's test-list command (from `architecture.md § Toolchain`) and record
the total test count. This baseline is compared in Step 4.

> [!TIP]
> Rust: `cargo test -- --list 2>&1 | Select-String "test$"` and count lines.
> If `architecture.md` does not define a test-list command, count test markers
> (e.g., `#[test]`, `test_`) via `grep_search`.


> [!WARNING]
> If validation fails, fix alignment issues before starting. Do NOT proceed
> with a misaligned task.md.

### 2. Git Checkpoint

> 📘 **Skill:** [`git-checkpoint`](.gemini/skills/git-checkpoint/SKILL.md) — atomic commit with quality gate verification

Ensure a clean working tree:

// turbo
```
git status --short
```

If the tree is dirty, commit or stash before starting. The first checkpoint
establishes the "before" state for the audit's `git diff`.

### 3. Step Execution Loop

> 📘 **Skill:** [`execute-plan-step`](.gemini/skills/execute-plan-step/SKILL.md) — full PARSE→READ→EXECUTE→VERIFY→UPDATE cycle

For each step in the plan's Global Execution Order, invoke the skill to perform the PARSE→READ→EXECUTE→VERIFY→UPDATE cycle.

#### MCP-Enhanced Implementation *(when available)*

During the Step Execution Loop, the Builder **SHOULD** use Narsil call-graph
tools for precision verification:

| Phase | Tool | Purpose |
|-------|------|---------|
| **READ** | `get_callers` | Verify the plan's stated caller count still holds. If new callers appeared since plan approval, STOP — the blast radius changed. |
| **CODE** | `get_callees` | When modifying a function, verify downstream contracts aren't violated by the change. |
| **TEST** (TDD) | `get_callees` | Ensure test cases cover all downstream call paths of the target function. |
| **VERIFY** | `get_complexity` | Confirm the modified function's complexity hasn't exceeded project thresholds. |


**When replacing a STUB:** Verify the new implementation satisfies the original contract
comment from the `// STUB(Phase N)` marker. The replacement must pass all existing tests
that exercised the stub.

**At 🔒 CHECKPOINT markers:**

> 📘 **Skill:** [`git-checkpoint`](.gemini/skills/git-checkpoint/SKILL.md) — atomic commit with quality gate verification

Invoke the skill.

> [!CAUTION]
> Do NOT skip checkpoints. They create atomic commits for the audit trail.
> All verification (fmt + clippy + test) must pass before committing.

#### Subagent Delegation *(Antigravity v2, L-tier with Parallel Lanes)*

When the approved plan declares Parallel Execution Lanes and multi-agent
orchestration is available:

1. The main Builder agent reads all lanes from the plan.
2. For each lane, delegate execution to a subagent, providing:
   - The lane's step range (e.g., Steps 1-3)
   - The lane's module boundary (e.g., `src/api/`)
   - The lane's sub-task file path (`task_lane_a.md`)
3. Each subagent follows the Step Execution Loop independently within its lane.
4. At the 🔒 SYNC CHECKPOINT, the main agent:
   - Merges all lane branches into the working branch
   - Runs the full `ALL` verification pipeline
   - Updates the main `task.md` with aggregated lane completion status
5. If any lane failed, the main agent STOPs and escalates to the Architect (using the standard escalation format and diagnostic file defined in `builder-rules.md` §4.5).

> [!NOTE]
> If multi-agent orchestration is NOT available, ignore this section entirely.
> The standard sequential Step Execution Loop applies.

### 4. Final Verification

> 📘 **Skill:** [`run-quality-gate`](.gemini/skills/run-quality-gate/SKILL.md) — run FMT + LINT + TEST pipeline

Invoke the skill. Confirm zero-exit on all gates.

**Test count regression check:**

Re-run the test-list command from Pre-Flight and compare to the baseline:

- **Count equal or higher** → proceed.
- **Count decreased** → check if the plan explicitly authorizes test deletion
  (look for `[DELETE]` or `[-]` on test files/functions in the GEO).
  - **Authorized** → proceed, note the delta in the Build Report.
  - **Unauthorized** → **STOP**. Log as `⚠️ Deviation: Test count regressed
    (baseline: N, current: M) without plan authorization.`


### 5. Build Report Generation

> 📘 **Skill:** [`generate-build-report`](.gemini/skills/generate-build-report/SKILL.md) — generate Act→Reflect handoff artifact

Invoke the skill. Follow its tiered logic and extraction rules to generate the report.

> [!NOTE]
> The report aggregates notes from task.md — it does not replace them.
> Builder Notes remain in task.md for traceability.

### 6. Completion

End the build with:

> ✅ **Act Phase Complete.** Reply with `/audit` for Reflect.

If a Build Report was generated, include a clickable link:
> [Build Report](file:///path/to/builder_report.md)

Do NOT proceed to audit yourself — the Architect role handles Reflect.

## Rules

1. **Follow `builder-rules.md`** — it defines scope discipline, fidelity hierarchy, and STOP conditions.
2. **TDD is non-negotiable** — tests first, even for minor changes. Every code change needs a test.
3. **Scope fence** — touch only files/functions listed in the current step. Minor additions per §4 are pre-approved.
4. **STOP on ambiguity** — if a step requires a design decision, halt and escalate to the Architect.
5. **No creative additions** — if you notice an improvement, write a Builder Note, don't write code.
6. **Update task.md** — Antigravity reads this for UI progress. Stale markers hide progress from the user.
7. **Git checkpoint at 🔒** — every checkpoint is a commit. No skipping.
8. **Wait for user instruction** before pushing to remote repositories.



