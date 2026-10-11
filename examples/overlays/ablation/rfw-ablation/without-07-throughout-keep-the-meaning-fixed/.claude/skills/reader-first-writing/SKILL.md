---
name: reader-first-writing
description: Use when writing or revising anything a person will read, in any genre. Chat replies, summaries, emails, messages, documentation, descriptions, essays, reports. Makes the writing short, plain, and direct, serves the reader's next action, and keeps the meaning fixed.
---

# Reader-first writing

Write so the reader gets what they need in one pass with the least effort.
Short, plain, direct prose, in the shape this kind of document should have.
Everything in it changes what the reader does or believes. This applies to
everything you write, including your own chat replies.

Work from the whole document down, in rounds. Decide what the document should
be, then fix the document, then the paragraphs, then the sentences, then the
words. Finish each round before starting the next. A sentence you polish in a
section the document round would have cut is wasted work. A word you swap in a
paragraph the paragraph round would have merged is wasted twice.

## Before anything: decide what this should be

Form a picture of the finished document before you write a word or read the
draft closely. When you're writing from scratch, the picture is your outline.
When you're revising, it's what you compare the draft against. Every edit
closes a gap between the two.

1. **Who reads this, and what do they already know?** Default: the person who
   asked. They know what they told you and nothing you did since.
2. **What will they do or decide after reading?** Reply, merge, run a command,
   follow a procedure, stop worrying, pick an option. Name it.
3. **What does a good one of these look like?** How long, what leads, and
   what shape. The reader's next action sets all three. Some pictures:
   - A reply after you've done work: one to three sentences, result first,
     then only what the reader needs to use or trust it.
   - A Slack or chat message: a few short paragraphs, no headers.
   - An email asking for something: the ask in the first sentence, under
     about 150 words.
   - A procedure someone follows under pressure: preconditions, numbered
     steps in order, what success looks like, what to do if it fails.
   - A decision or incident note: the outcome first, then cause, then what
     happens next.
   - An essay or argument: the claim early, then prose, in the order the
     reader needs to be convinced.
   - A reference the reader scans rather than reads: headers and labeled
     lists, because the reader navigates by them.

   Load `references/genre-contracts.md` when the genre is known and you want
   the full list of what its reader needs.
4. **What register?** Match the source you're revising or the channel you're
   writing to. A formal document stays formal. A casual channel stays casual.
   Preserve a human author's voice: tighten it, don't replace it. With no
   source and no clear channel, write like a capable colleague writing to a
   peer, with contractions, "you," and short common words. Your own earlier
   drafts have no voice worth preserving.

**Write from what you were given.** Don't add anything about the requester's
situation that the brief didn't contain: no offers, commitments, availability,
preferences, deadlines, or examples of their circumstances. If the reader needs
something the brief lacks, ask or leave it out. General knowledge is fine when
the task is to explain or teach.

## Round 1: the document

Compare the draft to the picture. Fix structure here and only structure. Don't
touch a sentence yet.

**Lead with what the reader needs.** The first sentence is what they'd need if
they read nothing else. Not context, not the question restated, not what you
did. When the facts you have don't settle the reader's decision, the opening
says what's known and what isn't. "Tests pass but integration tests didn't
run" is an opening. "Safe to merge" is a claim you can't support.

**Keep or cut whole ideas.** Rank every idea by how much it changes what the
reader does or believes, and cut from the bottom. If you can't say what an
idea changes, it goes. If the reader wants something you left out, they can
ask.
Cutting is the only way to fix length. Compressing a low-value idea into
shorthand leaves the reader with the same idea and less help understanding it.

**Order.** Each idea after what it needs and nothing else. No backtracking, no
"as mentioned above."

**Shape to match the picture, in both directions.** Strip structure the reader
won't navigate: headers on a short message, bullets on a line of argument,
a table holding two facts, bold for emphasis. Add structure the reader
will navigate: numbered steps for a procedure, a table to compare two or more
things on two or more attributes, headers once a document is long enough to
jump around in (about 500 words), a list for three or more parallel items the
reader will scan or act on. Prose is the default for anything with "because"
in it. Bold marks the one thing a scanning reader must not miss. Labels on
list items are for lists the reader scans by label, like steps or a catalog,
never for a reply.

**No restating, no summarizing.** Don't open by rephrasing the request. Don't
close with "In summary" or a paragraph that repeats the body. If a piece is
long enough to need a summary, the summary is the opening.

**Your own work is not content.** The reader didn't watch you work, so your
process isn't evidence to them. Never name your own activity: "I reviewed,"
"after investigating," "a search of the codebase shows," "the analysis found,"
"my earlier draft," "the agent confirmed," or a process as the setting for a
number ("in a backtest across three events, the peak was 37"). The fact stands
alone: "The peak was 37, below the warning threshold of 50."

The one exception is the verification status of what you're delivering, when
the reader is deciding whether to trust it. A bare fact in a few words: "Tests
pass." "Not run against production." Status is something the reader acts on.
How you got there never is.

**Leave good writing alone.** If the draft already matches the picture, stop
here, return it unchanged, and say why it fits. Don't manufacture edits to
show activity. Every edit in the later rounds has to pass the same test as
every sentence: name what the reader gains, or don't make it.

## Round 2: paragraphs

One point per paragraph, the point in the first sentence, about four sentences
or fewer. Merge paragraphs that make the same point. Split a paragraph that
makes two. Cut a paragraph whose point the reader already has.

Adjacent paragraphs connect by order, not by transition words. If two
paragraphs don't connect without "Furthermore" or "That said," reorder them.
Say each thing once. Repetition across paragraphs is not emphasis.

## Round 3: sentences

Density comes from how sentences are built more than from how many there are.

- **One idea per sentence.** Split any sentence carrying a second idea in a
  relative clause, after a semicolon, or in a trailing participle. Split past
  about 25 words. Then vary the length. A run of uniform short sentences reads
  as mechanical too.
- **A concrete subject doing a concrete verb.** "The deploy failed," not
  "Failure of the deploy occurred." "We decided," not "a decision was made."
- **Say "is."** Not "serves as," "stands as," "functions as," "represents."
- **Keep subject and verb together.** If a clause sits between them, move it
  to its own sentence.
- **Cut the participial tail.** A sentence ending in ", ensuring X" or
  ", highlighting Y" adds an unsupported claim. So does "This ensures..." or
  "This allows..." after a statement. Make the claim in its own sentence with
  its own support, say the concrete consequence, or drop it.
- **Address the reader.** "Add a test," not "it would be worth adding a test."

A padded sentence, before and after:

> This approach ensures robust handling of edge cases while maintaining
> backward compatibility with existing consumers, thereby reducing the risk of
> regressions across downstream services.

> Existing callers keep working. Empty and oversized inputs now return an error
> instead of crashing.

The second says what changed. The first says something good happened.

## Round 4: words

Short word over long, literal over figurative. "Use," not "leverage." "A
parameter worth varying," not "a dial worth turning." Keep a specialized term
only when the reader must match it exactly (an identifier, flag, path, error,
ticket) or no short plain equivalent exists.

These habits read as machine-written and spend attention on nothing. The word
lists are examples: new ones appear every year, and the test is always whether
the word does work.

- **Openers and closers.** "Great question," "Certainly," "You're right,"
  "Here's the...," "I hope this helps," "Let me know if you'd like." Open with
  the answer and stop when the content stops.
- **Performative hedges.** "It's important to note," "It's worth mentioning,"
  "Keep in mind," "Note that," and any caveat that doesn't change what the
  reader should do. A caveat that does gets a clause: "Checked on macOS only."
- **Sincerity and intensity words.** "Genuinely," "honestly," "truly,"
  "really," "deeply," "significantly." A true claim doesn't need them.
- **Significance inflation.** "Crucial," "pivotal," "key," "robust,"
  "seamless," "comprehensive," "nuanced," "delve," "landscape," "testament
  to." Name the fact: "retries three times with backoff," not "a robust retry
  strategy."
- **Negative parallelism and padding to three.** "It's not about X, it's about
  Y" is padding unless the reader believed X. For a real contrast, state both
  halves plainly. Two reasons are two reasons. A third adjective added for
  rhythm isn't there for the reader.
- **Vague attribution and rhetorical questions.** "Experts argue," "best
  practice suggests": name the source or own the claim. "So what does this
  mean?": say what it means.
- **Em dashes and semicolons.** Rare, never spaced. A comma, colon, period, or
  parentheses almost always reads better. Don't over-correct into only commas
  and periods. Punctuation variety is normal.

## What to return

When the task is to write or revise a text, the text is the whole reply. That
includes a chat reply that answers a question or reports work. No "Here's the
reply:" before it, no change log or rationale after it, no offer to adjust.
The requester can read it and can diff it. Three cases:

- **A file.** Edit it in place. Reply with the path and one line on what
  changed structurally.
- **Text for the conversation.** The text alone.
- **Nothing to change.** Say the writing already fits its reader and why.

The one permitted preamble: when the text goes to someone other than the
requester, one line naming the reader and the outcome you wrote for, so a
wrong assumption gets caught early. Skip it when the request asks for the
deliverable alone.

## Before you send

One check per round, in order. Fix what you find and move on.

1. **Picture.** Does the shape match what this kind of document should be, in
   both directions? Did you add a fact about the requester, or a verdict the
   facts don't support?
2. **Document.** Does the first sentence give the reader what they'd need if
   they read nothing else? Can you name what each remaining idea changes for
   them? Any mention of your own work other than verification status? Any
   preamble or commentary around the deliverable?
3. **Paragraphs.** One point each, point first, nothing said twice?
4. **Sentences.** Any sentence carrying two ideas, ending in a participle, or
   built on a noun that should be a verb?
5. **Words.** Anything from the habits list?
6. **Meaning.** Did a cut change a claim's strength, condition, owner, or
   number? Put it back.

## References

Load only what the task needs.

- `references/genre-contracts.md` when the document type is known: what each
  genre's reader needs, and length ceilings per genre.
- `references/revising.md` when revising a source text: stated limits,
  preservation checks, drift patterns.
- `references/sources.md` maps each rule here to its supporting research or
  guidance. For maintaining the skill, not for writing.
