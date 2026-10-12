# 7. Judge calibration

Value: medium. Difficulty: low-medium.

## Purpose

Rubric scores on the example writing tasks swung by about two points on a ten-point scale across three runs. The pairwise judge checks itself for position bias but nothing checks it against a person. The Review page collects human votes on the same pairs the judge voted on, and the reveal already shows the judge's pick, so the data for calibration exists and is thrown away.

Calibration tells the user which task categories the judge can be trusted on and when a human pass is still required.

## Requirements

1. Per launch, a calibration section in the report: the number of pairs with both a human and a judge vote, raw agreement (excluding human ties, skips and "both bad"), Cohen's kappa with a bootstrap interval, and agreement restricted to pairs where the judge was consistent across orders. Broken down by task tag when tags exist. Fewer than eight pairs reads "not enough human votes to calibrate".

2. Rubric scores are calibrated too: for each human-voted pair, the sign of the rubric score difference against the human winner gives a second agreement rate. This says whether the 1 to 10 scores track preference or only the pairwise votes do.

3. Across launches: `wrangler calibration [--judge m] [--rubric r] [--tag t]` pools every judgment in the project's database by judge model, rubric and task tag, and prints the same table. No schema change: judgments carry the launch, and the launch definition carries the judge configuration.

4. The review queue gets a calibration mode. `wrangler review --for calibration` (and a toggle on the Review page) prefers pairs that have a judge vote, spreading over tasks and tags, and within those the pairs where the judge was inconsistent or the rubric score difference was small. The default mode is unchanged.

5. Advice uses it. When a launch's agreement is below 0.7 or uncalibrated, every suggestion whose metric is `judge_score` or whose evidence is a pairwise judge result is downgraded one confidence step and gains the sentence "The judge is not calibrated on these tasks (agreement x on n pairs); review n more pairs." A `calibrate_judge` suggestion, priority 2, says how many pairs to review to reach eight per tag.

6. Suites record calibration with each baseline, so a suite diff can say whether the judge was trusted when the baseline was taken.

## Where it lands

- `crates/wrangler-core/src/stats.rs`: `kappa(pairs, seed)`.
- `crates/wrangler-core/src/report.rs`: the calibration section; `review.rs`: the queue mode; `advice.rs`: the downgrade and `calibrate_judge`.
- `crates/wrangler-core/src/suite.rs`: calibration summary stored beside the baseline (new column `calibration_json` on `baselines`, additive migration).
- `crates/wrangler-cli`: `calibration`, `review --for`.
- `apps/desktop`: calibration card on Results; mode toggle on Review.

## Acceptance

- Unit test: kappa on hand-built vote sets (perfect agreement, chance agreement, disagreement) and the interval behavior with few pairs.
- Unit test on the mock project: after scripted human votes that agree with the judge on two tags and disagree on a third, the report shows the split and advice downgrades suggestions on the third tag only.
- The review queue in calibration mode never offers a pair without a judge vote while one with a judge vote remains.

## Open questions

- Whether "both bad" votes should count as disagreement with any judge winner. Exclude them from agreement and count them separately; a high "both bad" rate is its own signal about the task.
