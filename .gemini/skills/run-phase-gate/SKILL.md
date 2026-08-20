---
name: run-phase-gate
description: >
  Verify that all multi-phase project exit criteria hold before advancing
  to the next planning and execution phase.
---

# Run Phase Gate

## When to Use
Invoked by `/audit` (as part of phase gate review), `/plan-making` (before multi-phase planning), and `/build` (stub verification).

## Constraints
- MUST verify ALL checklist items from `phase-rules.md §5`.
- MUST verify no **Blocking** tech debt remains.
- MUST verify all scheduled stubs were replaced.
- MUST run the full quality gate (ALL exits 0) on prior phase test suites.

## Procedure
1. **Completion Check:** Read `task.md` — verify all Phase N checklist items are `[x]`.
2. **Audit Check:** Verify the previous phase's audit verdict is ✅ Pass or ⚠️ Pass with notes (accepted).
3. **Debt Check:** Read `context.md` or debt logs to verify no `Blocking` tech debt remains unresolved.
4. **Stub Check:** Scan for stale stubs by running `rg "STUB\(Phase N\)"` in code. Verify 0 matches.
5. **Quality Check:** Invoke the `run-quality-gate` skill — ALL pipelines must exit 0, ensuring backward-compatibility against Phase N.
6. **Manifest Check:** Verify the Phase Manifest from Phase N is properly recorded in `context.md`.
7. **Next Phase Check:** Identify any stubs formally scheduled for Phase N+1 to inject into upcoming plans.
8. **Outcome Report:**
   - All pass → `"✅ Phase gate passed. Proceed to Phase N+1 planning."`
   - Blocking debt → `"❌ Blocking debt remains — address before planning."`
   - Stale stubs → `"❌ Stale stubs found — remediate or reschedule."`
   - Tests fail → `"❌ Prior tests failing — investigate regression."`
