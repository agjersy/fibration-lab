---
name: deck-contract
description: Explains what the deck checker enforces (template, fit, density, sources, accessibility, structure) and how to fix each finding. Use when check results come back with errors or warnings you are unsure how to resolve.
---

# The deck contract

After every accepted change the checker runs and returns findings. Errors must be fixed before you report the work as done. Warnings are judgment calls: fix them, or tell the user why you left them.

Fix the cause in the content. Never work around a finding by shrinking fonts, deleting citations, or moving text into speaker notes that the audience needs to see.

## Findings and fixes

| Rule | Means | Fix |
|---|---|---|
| `struct.titles` | A visible slide has no title, or two slides share one | Give each slide its own action title |
| `tpl.layout-only` | The layout is not in the template, or a free-positioned element is used where the project does not allow it | Choose a layout from `template_layouts`; put content in its placeholders |
| `tpl.theme-fonts`, `tpl.theme-colors` | A font or color is not from the theme | Remove the override; use theme color names |
| `tpl.mandatory` | A layout the organization requires (for example a closing or disclaimer slide) is missing | Add the slide with that layout |
| `fit.overflow` | Text does not fit its box | Shorten the text, move detail to notes, or split the slide |
| `fit.min-size` | Text only fits by shrinking below the minimum size | Same as overflow: less text |
| `density.bullets`, `density.words` | Too much on one slide | Cut to what proves the title; split if there are two messages; consider a chart, table, or diagram |
| `src.coverage` | A factual paragraph, chart, or table has no citation | Find the passage with `refs_search`, read it, and cite it; if nothing supports the claim, remove it or tell the user |
| `src.numbers` | A number in a cited run is not in the cited passage | Reread the passage with `refs_read`. Quote the figure as the source states it, cite the passage that does contain it, or cite both inputs of a derived value |
| `src.dangling` | A citation points at a reference, version, or location that does not exist | Search again and cite the current location |
| `src.chart-stale` | The reference behind a chart changed since the chart was computed | Preview the chart again and propose the update; check the title still holds |
| `a11y.alt-text` | A picture or chart has no alt text | Describe what it shows and why it matters, in a sentence |
| `a11y.contrast` | Text is hard to read against its background | Use the layout's default text color, or a darker or lighter theme color |
| `img.resolution` | An image is too small for the size it is placed at | Use a larger source or place it smaller |
| `out.opens-clean` | The built file does not open or convert cleanly | Report it to the user with the message; in code mode, check the last edit to the build script |
| `code.hardcoded-style` | A build script sets a literal color or font | Use the SDK's theme helpers and layout placeholders |

## Order of work

1. Errors that block the build (`out.opens-clean`, `tpl.layout-only`), since other findings may follow from them.
2. Source findings, because fixing them can change the text.
3. Fit and density, once the text is final.
4. Accessibility and images.

Run `check_run` on the slides you changed after each round, and render them to confirm the fix did not create a new problem. When a finding is caused by something you may not change (a locked slide, a template limit, a missing reference), say so plainly instead of forcing it.
