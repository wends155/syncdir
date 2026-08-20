---
name: web-fundamentals
description: >
  Universal baseline constraints for web-based UI output. Enforces accessibility (a11y), 
  semantic HTML structure, and core web vitals (CLS/LCP) independently of the framework.
---

# Web Fundamentals

## When to Use
This skill must be active alongside framework-specific skills (e.g., `svelte-patterns`) when generating or modifying any UI layer files, including:
- `.html`, `.svelte`, `.jsx`, `.tsx`, `.vue`
- Components in frontend/ directories.

## 1. The Semantic Strictness Protocol (No `div` Soup)
The Builder must prioritize structural HTML5 elements before resorting to `<div>` or `<span>`:
- Wrappers containing primary page elements must be `<main>`.
- Self-contained UI components (e.g., blog cards, product tiles) must be `<article>`.
- Distinct thematic groups of content must use `<section>`.
- Header depth (`<h1>` through `<h6>`) must not skip levels (e.g., no jumping from `<h2>` immediately to `<h4>`).

## 2. Visual Stability (CLS Defense)
Cumulative Layout Shift is treated as a fatal error.
- Every `<img>`, `<video>`, `<svg>`, or `<iframe>` tag **must** have explicit `width` and `height` attributes (or aspect-ratio CSS).
- Web fonts must use `font-display: swap` to prevent invisible text during load.

## 3. Paint Prioritization (LCP Optimization)
The Builder must explicitly designate the load priority of visual elements.
- **Above the fold:** The hero image or primary graphic must use `fetchpriority="high"` and `loading="eager"`.
- **Below the fold:** All other media defaults to `loading="lazy"` and `decoding="async"`.

## 4. Absolute Accessibility (A11y Zero-Exit)
Any interaction or visual information must be perceivable and operable by non-visual and keyboard-only users.
- **Icons & SVGs:** If an SVG has semantic meaning, it needs a `<title>` and `role="img"`. If it's purely decorative, it must have `aria-hidden="true"`.
- **Interactive Elements:** You may only append `onclick` or equivalent event handlers to naturally interactive elements (`<button>`, `<a>`, `<input>`). You may *not* bind click events to a `<div>` or `<span>` unless paired with `role="button"`, `tabindex="0"`, and keydown handlers for Enter/Space.
- **Labels:** Any input missing visible text must have an `aria-label` or `aria-labelledby`.

## Verification Gate
Outputs generated under this skill must pass structural linters. If the project's quality gate defines A11y linters (e.g., `eslint-plugin-jsx-a11y`) or Lighthouse checks, zero warnings are expected.
