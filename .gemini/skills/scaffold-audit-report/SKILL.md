---
name: scaffold-audit-report
description: >
  Generate a strictly compliant audit report skeleton by extracting the canonical template block from audit-rules.md.
---

# Scaffold Audit Report

## When to Use
Load this skill when you are the Architect tasked with drafting an Audit Report during the Reflect Phase (`/audit` workflow). 

## Procedure

1. **Target:** Read `.agents/rules/audit-rules.md` §1.
2. **Extract:** Mechanically extract the exact `<!-- TEMPLATE_START: audit-report -->` block. **Do not embed or duplicate the template contents**—rely entirely on the text extracted during this execution turn per `GEMINI.md §7 Rule 2`.
3. **Pre-populate Context:** Fill out the `Date`, `Scope`, and `Plan Reference` frontmatter variables automatically based on the ongoing transaction context.
4. **Generate Output:** Output the extracted template as the scaffold payload, maintaining exact column structure for the Audit Matrix, Build Artifact Validation, and Corrective Action tables.

