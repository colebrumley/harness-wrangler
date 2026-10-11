# Sources

What supports each rule in `SKILL.md`. For maintaining the skill, not for
writing. Not loaded during normal use.

Labels: **measured** means a corpus study found it; **vendor** means published
guidance from a model provider; **craft** means the clarity canon; **practice**
means editor and practitioner catalogs without frequency data.

## Why the skill is shaped the way it is

- **Before/after pairs and a stated positive target, not bare bans.**
  Anthropic's prompting guidance says to tell the model what to do instead of
  what not to do, and that examples are the most reliable way to steer tone and
  structure. Negated instructions get worse with model scale (Jang et al.,
  PMLR 2023, arXiv 2209.12711) and "don't mention X" fails under prompting
  alone (Castricato et al. 2024, arXiv 2402.07896). Contrastive positive and
  negative examples beat positive-only examples for response customization
  (arXiv 2401.17390). Explicit bans do hold for features vendors have trained to
  be steerable, like markdown and em dashes (Freeburg 2026, arXiv 2603.27006).
  So the skill uses pairs for habits and short explicit bans only for
  formatting and punctuation. [vendor, measured]
- **Thresholds as defaults.** Frontier models now follow concrete length and
  count instructions reliably (IFEval near saturation by mid-2026; OpenAI
  GPT-5.1 prompting guide, Nov 2025: "the model adheres well to concrete
  length guidance"). Sentence-length limits specifically are convention, not
  evidence (GOV.UK's 25-word rule, 2014, cites unlocated primary studies), so
  the skill uses them as split triggers, not targets. [vendor, craft]
- **A short rotating word list.** AI-overused vocabulary turns over roughly
  yearly: delve/tapestry in 2023-24, align with/fostering/showcasing in
  2024-25, emphasizing/enhance/highlighting in 2025-26, leverage/multifaceted/
  prioritize from mid-2026 (Wikipedia, Signs of AI writing; Juzek 2026, arXiv
  2605.25358). A fixed ban list rots. [measured, practice]
- **Trigger often.** Verbal tics roughly double between turn 1 and turn 20 of
  a conversation (Wu et al. 2026, arXiv 2604.19139), and OpenAI recommends
  re-asserting formatting rules every three to five turns. A skill that loads
  on most turns counteracts the decay. [measured, vendor]
- **Don't over-correct.** Vendors have trained out em dashes (Claude Opus 5 to
  5.5: 15.2 to 0.8 per thousand words, Arena data via BleepingComputer, Sep
  2026) and Wikipedia's editors now list uniform short comma-and-period
  sentences and reduced punctuation variety as a 2026 sign. Hence "vary the
  length" and "punctuation variety is normal." [measured, practice]

- **Rounds, from the whole document down.** Williams and Bizup order
  revision the same way: decide what the document is for, then its
  structure, then paragraphs, then sentences and words, because local
  polish on material that the structural pass removes is wasted. In this
  repository's own trials, a rule-catalog version of the skill produced
  word-level edits and flattened a runbook into a numbered list with no
  preconditions or rollback section. The rounds version, with an explicit
  "what should this look like" step, rebuilt the runbook shape and left the
  Slack message as prose. The picture step is what lets the skill add
  structure as well as remove it. [craft, practice]

## Decide what this should be

- Lead with the answer; point first in the paragraph, new information last in
  the sentence. Gopen and Swan, "The Science of Scientific Writing," American
  Scientist 1990; Williams, Style: Lessons in Clarity and Grace; Nielsen,
  "How Users Read on the Web," NN/g 1997 (79% scan; concise plus scannable
  plus objective text improved measured usability by 124%). OpenAI Model Spec
  (2026-08-18): a direct answer "rather than a list of facts." Anthropic Fable
  5.1 system prompt: identify the most important thing to say, then say that.
  [craft, vendor]
- Match length to the question. OpenAI Model Spec: "responsive in length."
  YapBench (arXiv 2601.00624, Jan 2026): excess length on brevity-ideal
  prompts varies by an order of magnitude across models; the failure modes are
  padding ambiguous inputs and explaining one-line asks. Why models run long:
  reward-model length bias (Dubois et al., LC-AlpacaEval, arXiv 2404.04475;
  Park et al. 2024 on DPO). [vendor, measured]
- Reply after work in one to three sentences, never a bare "Done." Claude
  Fable 5.1 system prompt (Sep 2026); GPT-5.1 guide: "lead with what you did."
  [vendor]

## Shape

- Minimum formatting; prose by default; lists for discrete parallel items;
  no headers in short answers; no bold for emphasis; no Title Case headings;
  no bolded-label bullet lists. Claude Fable 5.1 system prompt; Anthropic
  prompting docs (state when formatting is appropriate rather than a blanket
  ban, which over-suppresses on Fable 5.1); OpenAI Model Spec ("judiciously to
  aid the user in scanning"); GPT-5.1 guide ("only where semantically
  correct"); Wikipedia on mechanical boldface, Title Case, inline-header lists,
  Markdown residue. [vendor, practice]
- No closing summary or "In conclusion." Wikipedia (WP:CONCLUSION); Claude
  system prompts (no summaries unless asked). Practitioner consensus, not
  measured. [practice, vendor]
- Every sentence adds something; cut restatement, doubled words, and what the
  reader can infer. Zinsser, On Writing Well; Orwell, "Politics and the English
  Language," rule 3; Williams on concision. Fable 5.1 system prompt: "every
  word in Claude's response should mean something different and additive."
  OpenAI Model Spec: no "uninformative or redundant text." [craft, vendor]
- Paragraph length. plainlanguage.gov: paragraphs of three to eight sentences,
  under 150 words, never over 250. The skill's "about four sentences, about 100
  words" is tighter because the reader is usually on a screen. [craft]

## Sentences

- Nominalizations and hidden verbs. Reinhart et al., PNAS, Feb 2025 (arXiv
  2410.16107): instruct-tuned models use nominalizations at 1.5 to 2 times the
  human rate; Nieth et al. 2026 (arXiv 2605.23651) replicate across seven
  models and five registers. Williams, characters as subjects and actions as
  verbs; plainlanguage.gov on -ment, -tion, -sion, -ance. [measured, craft]
- Participial tails. Reinhart et al.: present-participial clauses at 2 to 5
  times the human rate (GPT-4o 5.3x), the single largest syntactic gap.
  Wikipedia: "superficial analyses" via sentence-final highlighting, ensuring,
  underscoring. [measured, practice]
- Copula avoidance ("serves as," "stands as"). Wikipedia catalog. [practice]
- That-clauses as subjects at 2.6 times the human rate. Reinhart et al.
  Covered by "a concrete subject doing a concrete verb." [measured]
- Subject and verb together; one idea per sentence; vary length. Gopen and
  Swan; plainlanguage.gov; Google Tech Writing One. Mean LLM sentence length is
  already short (Arena: 10 to 12 words), so density and uniformity are the
  problem, not raw length. [craft, measured]
- Passive voice is not an AI tell. Reinhart et al.: instruct models use
  agentless passive at about half the human rate. Williams: passive is a
  cohesion tool. The skill deliberately has no passive-voice rule. [measured,
  craft]
- Short word over long, literal over figurative. Orwell rules 2 and 5; Paul
  Graham, "Write Simply" (2021). Anthropic's Fable 5.1 prompting guide,
  "Writing density": mannered prose "substitutes metaphor and flourish for
  direct statement," with the pair "a dial worth turning" to "a parameter
  worth varying." [craft, vendor]

## Words and habits to drop

- Sycophantic openers and closers. Wu et al. 2026: "Absolutely!", "That's a
  great question!" are the most frequent English tics across eight frontier
  models; sycophancy correlates at r = -0.87 with human-rated naturalness.
  Wikipedia on collaborative residue ("I hope this helps," "Would you
  like..."). Claude system prompts. [measured, practice, vendor]
- "It's important to note" and generic disclaimers. Wu et al.: top-four tic.
  Wikipedia on didactic disclaimers. OpenAI Model Spec: avoid "excessive
  hedging"; Fable 5.1 prompt: caveats brief. [measured, vendor]
- Sincerity modifiers. Claude Fable 5.1 system prompt bans "genuinely,"
  "honestly," "straightforward" as disingenuous. Williams on intensifiers.
  [vendor, craft]
- Filler transitions. Wu et al. category "filler transitions" (Furthermore,
  Moreover, Additionally). Cherryleaf (Feb 2026) on "this means" signposting.
  Old-before-new ordering does the linking (Williams, Gopen and Swan).
  [measured, practice, craft]
- Significance inflation and promotional diction. Kobak et al., Science
  Advances, July 2025 (arXiv 2406.07016): excess style words in 15M PubMed
  abstracts, mostly verbs and adjectives (delves, underscores, intricate,
  pivotal). Liang et al., ICML 2024 (arXiv 2403.07183): meticulous 34.7x,
  intricate 11.2x, commendable 9.8x in ICLR 2024 reviews. Juzek and Ward,
  COLING 2025 (arXiv 2412.11385): RLHF, not pretraining data, explains the
  overuse. Wikipedia on "testament to," "pivotal role." OpenAI Model Spec:
  avoid "purple prose, hyperbole, clichéd phrases." [measured, practice,
  vendor]
- Negative parallelism. Wikipedia, Cherryleaf, and multiple 2025-26 editor
  guides agree. No corpus frequency found. One documented side effect: a
  blanket ban made a model drop content that was a real contrast (Habr, 2026,
  anecdotal), hence "unless the reader actually believed X." [practice]
- Padding to three. Wikipedia (WP:RO3), Cherryleaf, SilentRoom (Oct 2026).
  Widely shared, unquantified. [practice]
- Explaining the benefit ("This ensures..."). Overlaps measured participial
  tails and Cherryleaf's "this means" signposting. [measured, practice]
- Vague attribution. Wikipedia ("experts argue," "observers have cited");
  Cherryleaf's "generic specificity." [practice]
- Rhetorical questions as transitions. Cherryleaf. [practice]
- Em dashes. Freeburg 2026: 4 to 11 per thousand words in unconstrained
  essays from most frontier models against a human baseline of 3.2. Czuma 2026
  (arXiv 2608.05889): congressional press releases containing an em dash rose
  from 13% to 24.8% in 2025. Kosch et al. 2026 (arXiv 2609.23090): CHI papers
  dash density 1.21 to 2.08 from 2025 to 2026. Wikipedia WP:AIDASH. [measured,
  practice]
- Semicolons. Arena data: Claude Opus 5 to 5.5 went from 6.1 to 1.6 per
  thousand words, so this is now a trained-steerable feature. [measured]
- Markdown reflexes. Wikipedia (Title Case, emoji bullets, inline-header lists,
  Markdown residue, now the most common sign as of Oct 2026). Freeburg: a
  blanket anti-markdown instruction removed those features in all twelve
  models tested. [practice, measured]

## Your own work is not content

- Process narration is a form of the Wu et al. "pseudo-empathy" and filler
  categories and of Cherryleaf's "this means" signposting. The rule's
  strictness comes from this repository's own experience, not the literature.
  The verification-status exception follows Claude Code's own operating
  guidance to report test and verification outcomes plainly. [practice]

## Keep the meaning fixed

- Shortening raises claim strength. Peters and Chin-Yee, Royal Society Open
  Science, Apr 2025 (arXiv 2504.00025): across 4,900 summaries from ten
  models, 26% to 73% made broader claims than the source, 4.85 times the rate
  of human summaries, and accuracy prompting made it worse. Isch et al.,
  Nature Human Behaviour, Aug 2026: LLM summaries removed hedges and introduced
  causal language for associational findings; a caution prompt reduced it.
  SciZoom (arXiv 2603.16131): hedging words down 23% in ML abstracts across the
  LLM era. [measured]
- Models hedge in the wrong places. EMNLP 2025 (aclanthology
  2025.emnlp-main.1505): fail to hedge when unconfident, hedge when confident.
  NAACL 2025 (2025.naacl-long.452): LLM judges penalize epistemic markers,
  which rewards stripping them. Hence "hedge where you're uncertain, not where
  you aren't." [measured]
- Which cuts are safe. Williams: concision targets redundancy, inferable
  material, and phrase-for-word, and leaves assertions and qualifiers that
  change truth conditions. Anthropic Fable 5.1 guide on compaction: keep
  constraints, decisions, numbers, names, and exact wording; condense only the
  writer's own explanation. [craft, vendor]
- Drift pattern list. Original to this repository, consistent with the above.

## Register

- Match context; plain colleague as fallback. Anthropic prompting docs: the
  style of the prompt begets the style of the output. plainlanguage.gov:
  address the reader as "you." Graham: ordinary words, simple sentences.
  [vendor, craft]

## Gaps

No controlled study compares instruction forms (word lists versus exemplars
versus pairs versus thresholds) head to head. Triads, negative parallelism,
closing summaries, vague attribution, and uniform section balance are
practitioner consensus without corpus frequencies. Working-memory explanations
for sentence-length limits are weaker than usually claimed (Van Dyke et al.
2014; Caplan et al. 2011); the defensible rule is one idea per sentence, main
point first.
