---
name: executive-summary
description: Write or repair an executive summary slide that states the answer, the reasons, and the ask. Use when a deck needs a summary, when asked to shorten a deck to one slide, or when a summary merely lists the agenda.
---

# Executive summary

The executive summary is the deck in one slide for a reader who will read nothing else. Write it after the storyline is settled and revise it last, when the slides it summarizes are final.

## Content

1. **The answer.** The governing thought of the deck as the slide's title: a recommendation or a conclusion, as a full sentence.
2. **The reasons.** Three to five statements, one per main section of the deck, each a finding with its key number, in the order the deck presents them.
3. **The ask.** What you need from this audience: a decision, a budget, a date. If the deck only informs, state the implication instead.

## Rules

- Every statement appears, with its evidence, later in the deck. The summary introduces nothing new; if you need a new fact here, the deck is missing a slide.
- Every number matches the slide it comes from exactly, and carries the same citation. Read those slides with `deck_read_slide` first; do not work from memory.
- Findings, not topics: "Three suppliers account for 71% of late deliveries", not "Supplier performance".
- No agenda, no method, no background. Those have their own slides if they are needed at all.
- Keep within the layout's capacity: one line per statement where possible, two at most. If it overflows, the statements are too long, not the font too large.
- Use the template's summary layout if it has one; otherwise a title-and-content layout.

## Shortening a deck to a single slide

Keep the title as the answer, the strongest reason per section, and the ask. Drop qualifiers and secondary numbers. Say in your reply what was left out so the user can put it back.

## Check before proposing

- Read the title and statements aloud: do they stand alone for someone who has seen nothing else?
- Compare each number with its source slide.
- Run `check_run` on the slide and look at the render.
