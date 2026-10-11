# Revising a source text

Load this when there is an existing text to revise, shorten, restructure, or
polish. `SKILL.md` sets the writing rules. This file covers what is different
when the words started as someone else's.

## Revise the whole thing

Silence means revise everything, in the rounds `SKILL.md` lays out: picture,
document, paragraphs, sentences, words. You don't need permission to
restructure, and you don't need to keep the shape you were given. If the
organization is wrong, fix it. Don't leave a note saying it's wrong.

A perceived improvement never licenses a change of substance. Shorter and
clearer never means a different recommendation, a different level of certainty,
or a different scope.

## When the request limits the revision

A stated limit reaches exactly as far as it was stated:

- **"Don't cut anything."** Keep every idea. Reordering and restructuring are
  still yours.
- **"Just fix the grammar."** Keep every idea and the existing organization.
  Fix clarity, grammar, and local flow only.
- **"Leave section X alone."** Leave it, and revise the rest normally.

Don't widen a narrow limit into "leave it alone." Don't read politeness,
praise, or attachment to the draft as a limit.

## Leave good writing alone

When the source already fits its reader, return it unchanged and say so. Don't
manufacture edits to show activity. Changing "big" to "large" is not revision.

## Preserve meaning

Deletion may remove only material whose absence doesn't change what the reader
would believe, do, or owe. Everything you keep must keep its meaning. These are
the elements that carry it. Inventory them before a heavy edit:

- claims, conclusions, recommendations, instructions, and alternatives;
- certainty: must, should, may, likely, estimated, unknown;
- scope, exclusions, conditions, exceptions, causes, and evidence limits;
- risks, dependencies, sequencing, rollback, owners, and required actions;
- names, numbers, units, dates, times, quotations, citations, and sources;
- links, code, commands, paths, headings, tables, and deliberate terminology.

Keep the distinction between source fact, inference, recommendation, and open
question when the source draws it. Keep each element attached to its actor,
target, time, condition, and evidence.

Order, adjacency, grouping, transitions, and headings also carry meaning. Don't
use them to imply a causal, temporal, evidentiary, priority, or hierarchical
relationship the source doesn't establish. A new heading summarizes neutrally
and doesn't assign new status, ownership, or importance. When the relationship
between two ideas is unknown, separate them neutrally or say the gap exists.

Emphasis is meaning. What the source leads with stays prominent. Reordering for
the reader is expected. Demoting the lead claim to a subordinate clause or a
late bullet is not.

## How meaning drifts when text gets shorter

Each of these is common, and shortening makes all of them more likely:

- **Recommendation to instruction.** "We recommend enabling the flag after the
  test" becomes "Enable the flag after the test." A choice became an order.
- **Conditional to unconditional.** "Launch if Tuesday's rollback test passes"
  becomes "Launch this week." The gate disappeared.
- **Calibrated to certain.** "Evidence suggests Atlas owns it" becomes "Atlas
  owns it." An inference became a fact.
- **Correlation to cause.** "Errors rose after the deploy" becomes "The deploy
  caused the errors."
- **Scope loss.** "No action unless errors exceed 1%" becomes "No action
  needed." The exception disappeared.
- **Cause loss.** "Restart the loader because the connector freezes DNS at
  startup" becomes "Restart the loader." The reason that explains order and
  recovery disappeared.
- **Owner loss.** "Payments validates; SRE executes" becomes "The team
  validates and executes." Accountability changed.
- **Number and date drift.** "5% of traffic on Tuesday" becomes "a small pilot
  soon."
- **Source drift.** "The catalog names Atlas; git history supports that"
  becomes "Atlas is the confirmed owner." Two sources with different authority
  collapsed into one certainty.
- **Structural drift.** Independent observations land together under "Cause"
  or "Priority," implying a relationship the source never made.
- **Artifact drift.** A command, path, link, table cell, or code token changed
  while the prose around it was being improved.

Hedges are the biggest risk. Shortened text drops "may", "suggests", "in this
sample", "associated with" at high rates, and a cut that raises the strength of
a claim is a change of meaning. Cut performative hedges ("it's important to
note") and keep evidential ones.

## Verification pass

After editing, compare source and result twice. First check exact artifacts:
numbers, names, dates, commands, paths, links, quotes. Then read each claim
with its qualifiers and ask whether the result still says who does what, to
which scope, when, under what condition, why, with what confidence, and with
what consequence. Read across adjacent ideas and headings too, for implied
relationships the source didn't have.

If a required element is missing or changed, repair it. If the requested edit
can't be done without changing substance, say so instead of guessing.

Semantic equivalence isn't truth. If the task also asks for fact-checking, do
that as a separate pass, and don't change the source's claims during a style
edit because you believe an external fact. Say what you think is wrong.

## What to return

The revision alone, as `SKILL.md` says under "What to return": no preamble, no
change log, no rationale, no offer to adjust. Narrating your edits is both
commentary and self-reference, and it fails on both counts.

Ask a question only when the ambiguity changes the result and no default from
the source or the request covers it.
