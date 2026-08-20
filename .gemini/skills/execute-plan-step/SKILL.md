---
name: execute-plan-step
description: >
  Execute a single implementation plan step through the strict parse/read/execute/verify/update loop.
---

# Execute Plan Step

## When to Use
Invoked by the `/build` workflow at each step of the Global Execution Order.

## Constraints
*These constraints mirror `builder-rules.md` governance and must be obeyed.*
- MUST follow the Fidelity Hierarchy: Tests > Interfaces > Plan description > Code snippets.
- MUST NOT expand scope beyond the step header.
- MUST read target file before writing (Read Before Write).
- MUST run the Post check after each code change.
- MUST update `task.md` (`[ ]` → `[/]` → `[x]`) ONLY after verification passes.
- MUST STOP if the Pre-condition does not hold.
- MUST STOP on sub-tag scope guard violations (`[+]` must not exist, `[~]` must exist, `[-]` must exist).

## Procedure

1. **PARSE**
   Extract the file tag, function sub-tag, file path, function name, and line range from the step header.
2. **READ**
   Read the target file/function using `view_file`. Verify the Pre-condition holds. *If Pre-condition fails → STOP.*
   - *MCP Guidance:* Use `get_callers` to verify the caller count if specified in the plan. If new callers appeared, STOP (blast radius changed).
3. **EXECUTE (CODE)**
   Apply the change per the Action field.
   - For `[TEST]` steps: Write the test first (TDD Red). Sub-tag scope guards apply.
   - *MCP Guidance:* When modifying functions, use `get_callees` to verify downstream contracts aren't violated. For tests, ensure call path coverage.
4. **POST-VERIFY (Self-Verification Loop)**
   - Re-read changed lines using `view_file` or `grep_search`.
   - Compare changes against the Action description and Interface Contract.
   - Run the specified Post check command(s). *If Post fails → Enter Error Recovery.*
   - *MCP Guidance:* Use `get_complexity` to confirm complexity limits haven't been exceeded. 
5. **UPDATE**
   Mark `task.md`: `[ ]` → `[/]` → `[x]`

## Error Recovery
- **First failure:** Diagnose and fix the issue *within the current step's scope only*. Re-run the Post check.
- **Second consecutive failure (same step):** STOP immediately. Create a WIP commit.
- **NEVER fix forward:** Do not modify future step targets to work around a current failure.
- **Regression detection:** If previously-passing tests fail, STOP.
