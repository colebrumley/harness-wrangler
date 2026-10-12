# 6. Placebo overlays

Value: medium. Difficulty: low.

## Purpose

A measured gain from a skill or a `CLAUDE.md` may come from having instructions at all: any text that says "be careful" or restates the task changes a model's behavior a little. The ablation compares none, full and leave-one-out, which cannot separate "these words" from "some words". A placebo of matched length makes that split visible.

## Requirements

1. `wrangler ablate <source> --placebo` adds one variant named `placebo` to the plan. For a skill, the frontmatter (name and description) is kept so it triggers the same way, and the body is replaced. For a `CLAUDE.md`, the whole file is replaced. Replacement text matches the original's token count within ten percent, measured by the same estimator as `est_tokens` in the ablation report.

2. Replacement text is innocuous by construction: it restates behavior the model already has by default, in the same markdown shape (headings, bullets) as the original. A built-in corpus under `crates/wrangler-core/src/placebo/` supplies paragraphs; `--placebo-file <path>` substitutes the user's own. The corpus is reviewed for sentences that could plausibly change output and documented in `docs/formats.md` as what a placebo contains.

3. The report's ablation section shows `full vs placebo` beside `full vs none` and `placebo vs none`, each with an interval. A new attribution line for the whole file: `content` (full beats placebo), `presence` (placebo matches full and beats none), `neither` (nothing beats none), `inconclusive`.

4. Advice: `placebo_explains`, priority 1, when attribution is `presence`: "The gain comes from having instructions, not from this content; a shorter file would do." Evidence carries the three effects. `remove_section` and `keep_section` are unchanged; they already compare against full.

5. A placebo is also just an overlay. `wrangler ablate` writes it under `overlays/ablation/<exp>/placebo/` like the other variants, and nothing stops a user from putting `overlays/placebo` as a plain factor level in any experiment.

6. The example project's `rfw-ablation` and `plain-ablation` are regenerated with `--placebo`. The mock runner, which keys its behavior on words in the overlay, must not respond to the placebo corpus; the corpus avoids its trigger words and a test asserts this.

## Where it lands

- `crates/wrangler-core/src/ablation.rs`: the variant in `generate`, `AblationMeta.placebo`.
- `crates/wrangler-core/src/placebo/`: corpus and matcher.
- `crates/wrangler-core/src/report.rs`: the three effects and the whole-file attribution.
- `crates/wrangler-core/src/advice.rs`: `placebo_explains`.
- `crates/wrangler-cli`: the flags.
- `apps/desktop`: the Ablation view shows the placebo bar.

## Acceptance

- Unit test: the placebo for the example rfw skill keeps the frontmatter byte-identical, has a body within ten percent of the original's estimated tokens, and shares no sentence with it.
- Unit test on report data: `presence` attribution when placebo ≈ full > none; `content` when full > placebo ≈ none.
- The mock `plain-ablation` with `--placebo` reports `content` for the "Lead with impact" skill, since the mock responds to that phrase and not to the corpus.

## Open questions

- Whether a placebo should also be offered for `system_prompt_append` factors. Same mechanism, one more flag; do it if cheap once the overlay case works.
