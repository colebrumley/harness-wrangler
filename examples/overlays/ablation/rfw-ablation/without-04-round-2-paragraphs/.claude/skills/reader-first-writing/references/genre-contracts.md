# Genre contracts

Load this when the document type is known. Each entry says what that genre's
reader needs to find, in rough order of importance. These guide emphasis. They
are not required headings, and a missing concept doesn't need an empty section
or a placeholder. Include only what the source or task supports, and don't drop
a condition, owner, dependency, or caveat that is present to make the document
look lighter.

## Length ceilings

Ceilings, not targets. Exceed one only when a named reader can't complete a
named action without the extra material, never because the material is true,
hard-won, or interesting.

- Chat reply after doing work: one to three sentences, result first.
- Comment or review reply: three sentences.
- Commit message: a one-line subject under about 70 characters, then a body
  only when the why isn't obvious from the diff.
- PR or change description: about ten lines.
- Status update or standup note: five lines.
- Email or message asking for something: the ask in the first sentence, the
  whole thing under 150 words.
- Executive or leadership update: one screen, decision first.

## Genres

- **Chat reply:** the answer or result, then only what the reader needs to use
  or trust it. Verification status as a bare fact. No replay of steps.
- **Email or message:** what you need from the reader, by when, and the one
  piece of context that makes the ask reasonable. Anything else goes below a
  break or in an attachment.
- **Status update:** current state, significance, owner, and the next action or
  trigger. Make time, thresholds, uncertainty, and what is not needed easy to
  find.
- **Review reply:** the conclusion or requested change, the concrete reason,
  and evidence only when needed to resolve the review. Keep an unresolved
  choice visibly a choice. Prefer a complete, direct sentence over padding or
  shorthand: not "Great point! I've updated the retry logic as suggested. Let
  me know if you'd like anything else!", not "retry → exp backoff per cmt,
  a3f21c", but "Done. Switched to exponential backoff in a3f21c."
- **Commit message:** what changed and why, for someone reading history
  without the conversation. Imperative subject line.
- **PR or change description:** what changed and why it needed to. The reader
  is a reviewer deciding whether this diff is correct. Add rollout,
  compatibility, or risk detail only when it constrains merging. Adjacent
  problems found on the way, alternatives not taken, and anything the reviewer
  can't act on belong in the conversation or a ticket.
- **Documentation and how-to:** what the reader is trying to do, the steps in
  order, what success looks like, and what to do when it fails. Reference
  detail after the task, not before it.
- **Explanation or tutorial:** the one idea the reader should leave with, then
  the smallest example that shows it, then the complications. Prerequisites
  stated once at the top.
- **Essay or opinion piece:** the claim, early. Then the argument, in the order
  the reader needs to be convinced, not the order you thought of it. Address
  the strongest objection, not the weakest.
- **Report or research synthesis:** the answer to the question first. Then
  distinguish current evidence, older or conflicting evidence, inference,
  unknowns, and the practical recommendation.
- **Leadership update:** the decision or conclusion, its consequence,
  confidence and its basis, the main constraint or risk, and the decision or
  escalation needed from the reader.
- **Plan:** objective, recommended approach, what can proceed in parallel,
  dependencies and order, irreversible points, constraints, gates, owners, and
  open decisions.
- **Technical specification:** desired behavior, interfaces and invariants,
  compatibility and sequencing, rollout and rollback, failure behavior,
  acceptance criteria, and what's out of scope.
- **Change ticket:** intended outcome and scope, ordered steps, dependencies,
  validation, rollback or recovery, and accountable owners.
- **Cross-system coordination:** system roles, the contract or compatibility
  boundary, landing and deployment order, safe parallelism, activation gates,
  validation, ownership, and rollback coordination.
- **Handoff:** settled facts with where they came from, rejected options and
  why, unknowns, where the evidence lives, the next objective, what would
  reopen a settled question, and a clear stop point. Don't copy supporting
  detail the next person doesn't need to resume.

## When a human must decide or act

Put the decision or action first, as a plain sentence. Don't label it, box it,
or introduce it with a preamble.
