---
name: git-checkpoint
description: >
  Create atomic git commits at build checkpoints while verifying quality gates.
---

# Git Checkpoint

## When to Use
Invoked by `/build` at 🔒 CHECKPOINT markers in the Global Execution Order.

## Constraints
- MUST run the full quality gate (via `run-quality-gate` skill) before committing.
- MUST NOT skip checkpoints (they create atomic commits for the audit trail).
- MUST NOT chain `git add` and `git commit` in a single command.
- MUST update `task.md` before staging files.

## Procedure
1. Verify `task.md` is updated (all completed steps covered by this checkpoint must be marked `[x]`).
2. Invoke the `run-quality-gate` skill — all gates must pass with zero-exit.
3. Stage changed files: `git add <changed-files>`
4. Commit with message formatting: `git commit -m "step N-M: <description>"`
   *(where N-M represents the step numbers covered by this checkpoint)*
5. Confirm the commit succeeded.

## Error Recovery
- If the quality gate fails: Fix the issue within the current scope and re-run.
- If the commit fails: Diagnose (e.g., nothing to commit, lock file) and report.
