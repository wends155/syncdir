---
name: compress-context
description: >
  Compress recent interaction histories into the definitive context.md 
  file to manage context windows.
---

# Compress Context

## When to Use
Invoked by `/audit` (Step 6, Summarize) and `/update-doc` (Step 7, Summarize).

## Constraints
- MUST use the exact 4-field format (`Feature`, `Changes`, `New Constraints`, `Pruned`).
- MUST append to the existing `context.md` if it exists; create it if it doesn't.
- MUST record `Strategic Impact` when the change carries cross-project implications.

## Procedure
1. Gather compression data from the completed phase:
   - **Feature:** Target feature or core change module.
   - **Changes:** Brief summary of logic updated or files impacted.
   - **New Constraints:** Any newly emerged rules applicable to future Think phases.
   - **Pruned:** Definitively state what technical debt, temporary issues, or noise can now be ignored contextually.
2. Format the payload as the standard context update block:
   ```markdown
   ### YYYY-MM-DD: <Feature Name>
   - **Feature:** [Name]
   - **Changes:** [Summary]
   - **New Constraints:** [Rules]
   - **Pruned:** [Discarded context]
   ```
3. Load `context.md` with `view_file`.
4. Decide Write Mode:
   - **If `context.md` exists:** Prepend the block below the existing headers inside the `Interaction History` section using `multi_replace_file_content`.
   - **If `context.md` does not exist:** Create the file using `write_to_file`, populating it with this first entry structure.
