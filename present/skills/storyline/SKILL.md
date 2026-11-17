---
name: storyline
description: Turn a brief and reference material into a deck storyline before any slide is built. Use when asked for an outline, when a deck has no clear argument, or when the slide titles read in order do not tell the story.
---

# Storyline

A deck is one argument. Settle the argument before building slides; restructuring ten finished slides costs far more than rewriting ten titles.

## Steps

1. **State the governing thought.** One sentence that answers the audience's question: what should they believe or decide after the last slide? If you cannot write it, read more of the references or ask the user. Do not start with slides.
2. **Find the three to five supporting points.** Each must be necessary to the governing thought, and together they must be enough. Check them against the references with `refs_search`: a supporting point with no evidence is an opinion, so drop it or tell the user what is missing.
3. **Order them for this audience.** Answer first for decision makers (recommendation, then why, then what it takes). Situation, complication, resolution when the audience does not yet agree there is a problem. Chronological only when the sequence itself is the point.
4. **Write one action title per slide.** A full sentence that states the slide's message with its key number where there is one: "Churn fell to 2.1% after the onboarding redesign", not "Churn analysis". One message per slide.
5. **Run the title test.** Read only the titles, in order. They should make the argument without the slide bodies. Fix gaps (a step in the logic is missing), repeats (two slides with one message), and orphans (a slide that supports nothing).
6. **Propose it** with `outline_propose`: for each row the title, the message in a sentence, the layout you expect to use, and the citations that will carry it.

## Shape

- Open with the answer: an executive summary slide directly after the title slide for any deck longer than about six slides.
- Section dividers only when a section has three or more slides.
- Close with what happens next: a decision, owners, and dates. Not "Thank you", unless the template requires a closing layout.
- Appendix material (method, detailed tables) goes after the close and is referred to from the slide it supports.

## Sizing

A slide carries one message and takes one to two minutes to present. A 20-minute slot is 10 to 12 content slides, not 30. If the brief gives a time or slide limit, plan to it and say what you left out.

## Common faults

- Titles that are topics ("Market overview") rather than messages.
- A storyline that follows the order of the source documents instead of the order of the argument.
- A point the references do not support, kept because it sounds right.
- Background first, answer last, for an audience that asked a direct question.
