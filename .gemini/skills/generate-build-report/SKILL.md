---
name: generate-build-report
description: >
  Generate the structured Act→Reflect handoff artifact (builder_report.md) for the Architect.
---

# Generate Build Report

## When to Use
Invoked by `/build` at Step 5 (Build Report Generation).

## Constraints
- MUST extract the build-report template from `builder-rules.md §10 TEMPLATE_START` block per `GEMINI.md §7 Rule 2` (templates stay in rule files).
- MUST NOT reconstruct the template from memory.
- MUST apply the tiering gate (S-tier is optional; M/L are mandatory).
- MUST include the artifact link in the chat response.

## Procedure
1. Check the plan tier (`S/M/L`) from the plan header.
2. Apply Tiering Gate:
   - **S-tier:** Generate ONLY if Builder Notes exist in `task.md`. Otherwise, end workflow.
   - **M/L-tier:** ALWAYS generate report.
3. Read `.agents/rules/builder-rules.md` and extract the `<!-- TEMPLATE_START: build-report -->` block directly.
4. Read `task.md` and extract all entries under `## Builder Notes`.
5. Categorize the extracted notes:
   - Notes prefixed with `⚠️ Deviation:` migrate to the **Deviations** table.
   - All other notes migrate to the **Builder Notes** section.
6. Record the verification results (FMT + LINT + TEST) achieved in Final Verification.
7. Discover all files touched in this step's build via `git show --name-only --format="" HEAD` and list them.
8. If error recovery was triggered at any point, record the instance in the Error Recovery Log.
9. Write `builder_report.md` to the artifacts directory using the `write_to_file` tool (with `IsArtifact: true`).
10. Reply in chat with a clickable artifact link (e.g., `[Build Report](file:///path/to/artifacts/builder_report.md)`).

## Error Recovery
If the `TEMPLATE_START: build-report` extraction fails (e.g., missing template in rules), STOP and report. Do NOT generate the report structure from memory.

