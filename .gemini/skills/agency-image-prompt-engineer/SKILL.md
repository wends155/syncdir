---
name: agency-image-prompt-engineer
description: Structured prompt engineering framework for professional AI image generation
---

# Image Prompt Engineer

## When to Use
Load this skill during the Foundation & Structure phase of the `/design` workflow (Step 2) when Assets mode is active, to structure `generate_image` calls.

## 5-Layer Prompt Structure
Every prompt should specify:
1. **Subject Description Layer**: Primary subject, specific attributes, interactions, scale
2. **Environment Layer**: Location type, background treatment, atmospheric conditions
3. **Lighting Layer**: Light source, direction, quality, color temperature
4. **Technical Layer**: Camera perspective, focal length, depth of field, exposure
5. **Aesthetic Layer**: Photography genre, era, post-processing style

## Genre-Specific Patterns
- **Portrait**: [Subject details] | [Pose] | [Background] | [Lighting setup] | [Camera specs] | [Editorial style]
- **Product**: [Product details] | [Surface/Backdrop] | [Softbox lighting] | [Macro/Standard camera] | [Hero shot context]
- **Landscape**: [Geological features] | [Time/Atmosphere] | [Elements] | [Wide-angle specs] | [Light direction]
- **Fashion**: [Model/Wardrobe] | [Hair/Makeup] | [Set design] | [Pose type] | [Mixed lighting] | [Magazine style]

## Platform Optimization Hints
- **Midjourney**: Use parameter flags (`--ar 16:9`, `--stylize 750`, `--chaos 20`). Use multi-prompt weighting with `::` syntax. Append `--v 6` for latest model. Negative prompts via `--no`.
- **DALL-E**: Use natural language optimization — be descriptive and conversational. Specify style mixing explicitly (e.g., "in the style of editorial photography"). Quality via `quality: hd`.
- **Stable Diffusion/Flux**: Use exact token weighting `(subject:1.3)` for emphasis. Emphasize photorealism through technical camera terminology. Use negative prompts extensively for artifact control.

## Wireframe Prompt Scaffold

### When to Use
Load this section during **Step 1.5** of the `/design` workflow (GUI mode) to generate uniform, low-fidelity wireframes before styled mockup generation. This section is loaded separately from the 5-Layer Prompt Structure — wireframes use their own template, not the photography-oriented layers above.

### GUI Wireframe Base Prompt Template

Fill all `[VARIABLE]` slots from the Design Brief before submitting to `generate_image`:

```text
UX/UI wireframe, low-fidelity structural blueprint, flat monochrome design.
Background: white (#FFFFFF). Elements: light gray (#E5E5E5) fills with dark gray (#333333) borders.
Primary CTA elements: highlighted with [ACCENT_COLOR] fill.
[DATA_DENSITY] spacing (tight/balanced/spacious).
Clean [COLS]-column grid layout visible.
Viewport: [WIDTH]x[HEIGHT]px, [DEVICE] orientation.
No readable text — use greeked placeholder blocks for body copy.
Labels use short placeholder words (e.g., "Title", "Button", "Nav Item").
All interactive elements annotated with thin-border rectangles.

Screen purpose: [SCREEN_PURPOSE]
Navigation: [NAV_PATTERN] with items: [NAV_ITEMS]
Layout: [LAYOUT_DESCRIPTION]
Components: [COMPONENT_LIST]
```

### Variable Reference

| Variable | Source | Default |
|----------|--------|---------|
| `ACCENT_COLOR` | Design Brief → Primary brand color | `#4A90D9` |
| `DATA_DENSITY` | Design Brief → Data Density | `balanced` |
| `COLS` | Inferred from layout complexity | `12` (dashboard), `8` (content), `4` (mobile) |
| `WIDTH x HEIGHT` | Design Brief → Target viewport | `1440x900` |
| `DEVICE` | Design Brief → Device type | `desktop` |
| `SCREEN_PURPOSE` | Design Brief → Primary Objective for this screen | — |
| `NAV_PATTERN` | Design Brief → Navigation & Menu Structure | — |
| `NAV_ITEMS` | Design Brief → Main Menu Items | — |
| `LAYOUT_DESCRIPTION` | Agent-composed from requirements | — |
| `COMPONENT_LIST` | Agent-composed from requirements | — |

### Mockup Upgrade Reference

After wireframes are **"Structure Approved"**, switch to the **Mockup Upgrade Prompt** in `design-rules.md` §3.5. That prompt embeds the Wireframe Structure Description verbatim and layers design tokens on top — do NOT use this wireframe template for styled mockup generation.

