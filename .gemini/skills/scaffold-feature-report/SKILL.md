---
name: scaffold-feature-report
description: >
  Generate a strictly compliant feature research report skeleton by extracting the canonical template block from feature-rules.md.
---

# Scaffold Feature Report

## When to Use
Load this skill when you are the Architect tasked with drafting a Feature Research Report during the Pre-Think Phase (`/feature` workflow). 

## Procedure

1. **Target:** Read `.agents/rules/feature-rules.md` §2.
2. **Extract:** Mechanically extract the exact `<!-- TEMPLATE_START: feature-research-report -->` block. **Do not embed or duplicate the template contents**—rely entirely on the text extracted during this execution turn per `GEMINI.md §7 Rule 2`.
3. **Pre-populate Context:** Fill out `Feature name`, `Category`, `Component`, `Priority`, and `Filed` attributes automatically based on your initial investigation and classification steps.
4. **Generate Output:** Output the extracted template as the scaffold payload, maintaining exact headers for Business/Value Proposition, User Stories / Acceptance Criteria, Technical Investigation, Architecture Impact, Dependencies, Out of Scope, and Phased Implementation Strategy.

