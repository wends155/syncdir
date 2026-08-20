---
name: validate-task-alignment
description: >
  Cross-check task.md against the implementation plan to ensure all 
  file modifications are perfectly synchronized before execution.
---

# Validate Task Alignment

## When to Use
Invoked by `/build` (Pre-Flight, Step 1) and `/plan-making` (Pre-Flight Gate, Step 4 check #5).

## Constraints
- MUST read both `task.md` and the implementation plan file using `view_file`.
- MUST report all mismatches — do not silently skip.
- MUST STOP if mismatches are found (do not proceed with a misaligned `task.md`).

## Procedure
1. Read `task.md` with `view_file`.
2. Read the specific plan file (e.g., `artifacts/implementation_plan.md`) with `view_file`.
3. Extract all explicitly enumerated file entries marked `[NEW|MODIFY|DELETE|TEST]` from the plan.
4. Extract all mapped checklist entries from `task.md`.
5. **Forward Check:** Verify every extracted file entry from the plan acts as a checklist item in `task.md`.
6. **Reverse Check:** Verify every file entry in `task.md` corresponds directly to a stated plan entry.
7. Report the alignment results:
   - If perfectly aligned: `"✅ task.md aligned with plan"`
   - If mismatches exist: List the missing/extra entries and STOP.
