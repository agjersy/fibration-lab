---
name: chart-selection
description: Choose the right native chart, table, or diagram for a slide's message and shape its data with transform steps. Use before creating any chart, when a chart is hard to read, or when deciding between a chart and a table.
---

# Chart selection

Start from the slide's title. The chart exists to prove that sentence; pick the form that makes it visible at a glance, and remove everything that does not help.

## From message to form

| The title says | Use | Notes |
|---|---|---|
| One value differs from the others (ranking, comparison across categories) | Bar, sorted by value | Horizontal bars when labels are long or there are more than about seven categories |
| Something changed over time | Line (many periods) or column (few periods) | Time on the horizontal axis, oldest first |
| Parts make up a whole | Stacked bar, or 100% stacked across groups | Pie or doughnut only for two or three parts at one point in time |
| Two measures move together | Scatter | Say what the relationship is in the title |
| A total is built up or broken down step by step | Stacked column per step, or a table when exact values matter | Sort steps in the order the argument walks through them |
| The reader needs exact values, or there are several measures per item | Table | Up to about six columns and eight rows; more belongs in an appendix |
| Steps, phases, a two-by-two, or a reporting structure | Diagram: `process`, `timeline`, `matrix`, `hierarchy` | You give the content; the app does the layout |
| One number is the message | Large text in the layout's placeholder, with its citation | No chart needed |

## Shaping the data

Charts take their data from a reference table or range (`refs_table` shows it) through declarative steps, which the app runs and records with the chart. Preview with `chart_preview` before you propose the slide.

- **Reduce first.** `filter` to what the title is about; `top_n` with an "Other" row when there are many small categories.
- **Aggregate to the level of the message.** `group_sum` or the other group steps; a quarterly message does not need monthly points.
- **Order deliberately.** `sort` by value for rankings; keep natural order for time and for ordered categories.
- **Derive honestly.** `pct_change`, `share_of_total`, `cumulative`, `scale`, and `round` are available, as are `pivot` and `unpivot` for reshaping. Do not compute values yourself and type them in: a typed number cannot be traced or rechecked.
- Limits that keep charts legible: about six series, about twelve categories. Past that, split the slide or use a table.

For analysis beyond these steps (regressions, forecasts, joins across sources), the work belongs in Fibration Lab; import its figure or table as a reference.

## Finishing

- The title states the finding; the chart title, if any, states what is plotted and the unit.
- Label directly where the template allows it and drop the legend for a single series.
- Start value axes at zero for bars and columns.
- Use theme colors only. To emphasize one series or bar, give it an accent color and leave the rest neutral.
- Every chart cites the range or table it plots and has alt text that states the finding, not "bar chart".
- Render the slide and look: are labels cut off, do they overlap, can the smallest text be read?
