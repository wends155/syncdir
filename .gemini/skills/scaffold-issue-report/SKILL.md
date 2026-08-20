---
name: scaffold-issue-report
description: >
  Generate a strictly compliant issue report skeleton by extracting the canonical template block from issue-rules.md.
---

# Scaffold Issue Report

## When to Use
Load this skill when you are the Architect tasked with drafting an Issue Report during the Pre-Think Phase (`/issue` workflow). 

## Procedure

1. **Target:** Read `.agents/rules/issue-rules.md` §2.
2. **Extract:** Mechanically extract the exact `<!-- TEMPLATE_START: issue-report -->` block. **Do not embed or duplicate the template contents**—rely entirely on the text extracted during this execution turn per `GEMINI.md §7 Rule 2`.
3. **Pre-populate Context:** Fill out the `Type`, `Component`, `Severity`, and `Filed` attributes automatically based on your initial investigation and classification steps.
4. **Generate Output:** Output the extracted template as the scaffold payload, maintaining exact headers for Investigation Log, Root Cause Analysis, Blast Radius, Proposed Fix, and Builder Notes.

