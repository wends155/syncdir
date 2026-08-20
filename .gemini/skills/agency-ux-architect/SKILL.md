---
name: agency-ux-architect
description: Layout decision frameworks, design token vocabulary, and navigation patterns for structural design
---

# UX Architect

## When to Use
Load this skill during the Foundation & Structure phase of the `/design` workflow (Step 2) when GUI or Assets mode is active, to apply structural, layout, and token decisions to mockups.

## Design Token Vocabulary
*(Composed from CSS architecture principles)*

| Category     | Purpose / Description | Examples |
|--------------|-----------------------|----------|
| Colors       | Semantic mapping of the palette | Primary, Secondary, Accent, Neutral, Semantic |
| Spacing      | Base unit and multiplier scale | Base unit (4px), increments (xs, sm, md, lg, xl) |
| Typography   | Font families, sizes, and weights | Heading family, Body family, Size scale, Weight scale |
| Borders      | Corner geometry and line thickness | Radius (sm, md, pill), Width (1px, 2px) |
| Shadows      | Depth and elevation strategy | Level 1 (subtle), Level 2 (hover), Level 3 (modal) |
| Transitions  | Motion timing and easing curves | Default easing (ease-out), Duration (200ms) |

## Layout Decision Framework
- **Container System**: Define max-width constraints (Mobile: full, Tablet: 768px, Desktop: 1024px, Large: 1280px).
- **Grid Patterns**: Establish section constraints (e.g., Hero: full viewport, Content: 2-column desktop/1-column mobile, Cards: auto-fit).
- **Component Hierarchy**: Separate Layout components (containers) from Content components (cards) from Interactive components (buttons).

## Navigation Taxonomy
*(Composed from UX architecture principles)*
- **Sidebar (Dashboards)**: Best for complex, deeply nested application structures.
- **Top-bar (Marketing)**: Ideal for high-level site discovery and branding focus.
- **Tabs (Settings)**: Excellent for switching views within the same context without losing state.
- **Breadcrumb (Deep Hierarchy)**: Necessary for contextual awareness in nested data.
- **Command Palette (Power Users)**: High-efficiency keyboard navigation for complex tools.

## Visual Weight System
- **Hero**: Primary page title, largest text, boldest weight, highest contrast.
- **Section**: Section headings (H2), medium emphasis, separates content blocks.
- **Body**: Standard text size, readable, sufficient contrast, comfortable line-height.
- **Caption**: Smallest text, muted colors, used for secondary or tertiary labels.
