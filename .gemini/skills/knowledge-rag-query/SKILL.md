---
name: knowledge-rag-query
description: >
  Convention for querying the global knowledge-rag instance before falling
  back to Context7 or web search. Applies to all Builder sessions.
---

# Knowledge-RAG Query Convention

## When to Use
Load this skill when the **Builder or Architect** needs information about:
- Dependency APIs, usage patterns, or common mistakes
- Language-specific DOs/DON'Ts/gotchas (Rust, TypeScript)
- TARS workflow or rule discovery
- Best-practice validation during `/audit` compliance checks (§2f)

## Query-First Protocol

1. **Query knowledge-rag FIRST** using the `search_knowledge` tool
   with a targeted natural language query.
   - Example: `"tokio spawn async task timeout pattern"`
   - Example: `"Rust borrow checker gotcha lifetime elision"`

2. **Evaluate response quality:**
   - ✅ **2+ relevant chunks** → Use the retrieved context. Done.
   - ⚠️ **0–1 chunks or low relevance** → Escalate to Context7.

3. **Fallback to Context7** using `resolve-library-id` → `query-docs`:
   - Log: `"knowledge-rag miss on {topic} → escalating to Context7"`

4. **Last resort: web search** — only if both knowledge-rag and
   Context7 fail to provide sufficient information.

## Namespaces

When querying, mentally scope to the relevant namespace:

| Need | Query prefix hint |
|:---|:---|
| Rust dependency docs | `"crate:{name} ..."` |
| TypeScript dependency docs | `"package:{name} ..."` |
| Rust language patterns | `"rust pattern ..."` or `"rust gotcha ..."` |
| TypeScript patterns | `"typescript pattern ..."` |
| TARS workflow discovery | `"workflow ..."` or `"rule ..."` |

## Constraints

- Do NOT use knowledge-rag as a replacement for `view_file` on
  GEMINI.md, architecture.md, or rule files during workflow
  prerequisites (GEMINI.md §7 Rule 1 still applies).
- Knowledge-rag is for **discovery and reference**, not for
  loading mandatory governance documents.
- If the retrieved content seems outdated (e.g., wrong version),
  escalate to Context7 or web search and flag for re-ingestion.
