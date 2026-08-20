---
name: agency-ui-designer
description: Component taxonomy, state documentation, and design system structure for systematic UI design
---

# UI Designer

## When to Use
Load this skill during the Foundation & Structure phase of the `/design` workflow (Step 2) when GUI or Assets mode is active, to define component hierarchies and states.

## Component Taxonomy
*(Composed using atomic design methodology)*
- **Atoms**: Irreducible base elements (button, input, label, icon)
- **Molecules**: Simple groups functioning together (form field, search bar, card)
- **Organisms**: Complex, distinct sections of an interface (header, sidebar, data table)
- **Templates**: Page-level wireframe structures without content
- **Pages**: Final assembled views with real content

## Design System Structure
*(Composed from systematic UI principles)*
- **Tokens** → **Components** → **Patterns** → **Layouts**
- *Naming Convention Guidance*: Always use semantic descriptions (e.g., "Primary", "Warning", "Subtle") instead of visual descriptions (e.g., "Blue", "Red", "Light").

## State Documentation Matrix
Ensure all interactive components account for the following states:
- Default (Base resting state)
- Hover (Mouse over indicator)
- Active/Pressed (Interaction confirmation)
- Focus (Keyboard accessibility outline)
- Disabled (Inactive and unclickable)
- Error (Validation feedback)
- Loading (Action in progress)
- Empty/Placeholder (No data state)

## Responsive Framework
- **Mobile First Approach**: Best for highly constrained content or rapid consumer iteration. Design for 320px-639px baseline, then scale up.
- **Desktop First Approach**: Useful for complex B2B dashboards. Base design at 1024px+, then stack elements for mobile later.
- **Standard Breakpoints**: Mobile: 320-639px, Tablet: 640-1023px, Desktop: 1024-1279px, Large: 1280px+
