//! Placebo overlays: text of matched length and Markdown shape that
//! restates what an agent does by default. Comparing a pattern against its
//! placebo separates "these instructions" from "some instructions".

use crate::ablation::split_frontmatter;
use crate::util::est_tokens;

/// The built-in corpus. `docs/formats.md` says what it contains and why.
pub const CORPUS: &str = include_str!("corpus.md");

/// How close the placebo's estimated tokens must be to the original's.
pub const TOLERANCE: f64 = 0.10;

#[derive(Debug, Clone)]
pub struct Corpus {
    pub headings: Vec<String>,
    pub sentences: Vec<String>,
}

impl Corpus {
    /// Parse a corpus file: sentences one per line under `## Sentences`,
    /// headings under `## Headings`. A file with neither section is taken
    /// as sentences, one per line, with the built-in headings.
    pub fn parse(text: &str) -> Corpus {
        let mut section = "";
        let (mut headings, mut sentences, mut loose) = (vec![], vec![], vec![]);
        for line in text.lines() {
            let t = line.trim();
            if let Some(h) = t.strip_prefix("## ") {
                section = match h.trim().to_lowercase().as_str() {
                    "headings" => "headings",
                    "sentences" => "sentences",
                    _ => "",
                };
                continue;
            }
            if t.is_empty() || t.starts_with('#') {
                continue;
            }
            match section {
                "headings" => headings.push(t.to_string()),
                "sentences" => sentences.push(t.to_string()),
                _ => loose.push(t.to_string()),
            }
        }
        if sentences.is_empty() {
            sentences = loose;
        }
        if headings.is_empty() && !std::ptr::eq(text, CORPUS) {
            headings = Corpus::builtin().headings;
        }
        Corpus { headings, sentences }
    }

    pub fn builtin() -> Corpus {
        Corpus::parse(CORPUS)
    }
}

/// Draws sentences and headings in order, wrapping around.
struct Draw<'a> {
    corpus: &'a Corpus,
    sentence: usize,
    heading: usize,
}

impl Draw<'_> {
    fn sentence(&mut self) -> &str {
        let s = &self.corpus.sentences[self.sentence % self.corpus.sentences.len()];
        self.sentence += 1;
        s
    }
    fn heading(&mut self) -> &str {
        let s = &self.corpus.headings[self.heading % self.corpus.headings.len()];
        self.heading += 1;
        s
    }
    /// The sentence among the next few whose length is closest to `target`.
    fn fitting(&mut self, target: usize) -> String {
        let n = self.corpus.sentences.len();
        let best = (0..n.min(12)).min_by_key(|k| self.corpus.sentences[(self.sentence + k) % n].len().abs_diff(target)).unwrap_or(0);
        let s = self.corpus.sentences[(self.sentence + best) % n].clone();
        self.sentence += 1;
        s
    }
    /// Sentences until the text is about as long as `target` characters.
    fn about(&mut self, target: usize) -> String {
        let mut out = String::new();
        loop {
            // A short slot (a terse bullet) gets the best-fitting sentence, not merely the next one.
            let next = if out.is_empty() { self.fitting(target) } else { self.sentence().to_string() };
            if !out.is_empty() && out.len() + next.len() / 2 > target {
                self.sentence -= 1;
                break;
            }
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(&next);
            if out.len() >= target {
                break;
            }
        }
        out
    }
}

fn bullet_prefix(line: &str) -> Option<&str> {
    let t = line.trim_start();
    let indent = line.len() - t.len();
    for marker in ["- ", "* ", "+ "] {
        if t.starts_with(marker) {
            return Some(&line[..indent + 2]);
        }
    }
    let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 && (t[digits..].starts_with(". ") || t[digits..].starts_with(") ")) {
        return Some(&line[..indent + digits + 2]);
    }
    None
}

/// Replace a Markdown text with placebo text in the same shape: a heading
/// for each heading (same level), a bullet for each bullet, a paragraph for
/// each paragraph, each about as long as what it replaces. Fenced code
/// blocks become paragraphs. The result's estimated token count is within
/// `TOLERANCE` of the original's.
pub fn placebo_text(original: &str, corpus: &Corpus) -> String {
    assert!(!corpus.sentences.is_empty() && !corpus.headings.is_empty(), "placebo corpus is empty");
    let mut draw = Draw { corpus, sentence: 0, heading: 0 };
    let mut blocks: Vec<String> = vec![];
    let mut para = 0usize; // characters of the paragraph being accumulated
    let mut fence: Option<&str> = None;
    let flush = |para: &mut usize, blocks: &mut Vec<String>, draw: &mut Draw| {
        if *para > 0 {
            blocks.push(draw.about(*para));
            *para = 0;
        }
    };
    for line in original.lines() {
        let t = line.trim_start();
        if let Some(f) = fence {
            para += line.len() + 1;
            if t.starts_with(f) {
                fence = None;
            }
            continue;
        }
        if t.starts_with("```") || t.starts_with("~~~") {
            fence = Some(&t[..3]);
            para += line.len() + 1;
            continue;
        }
        let hashes = t.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&hashes) && t[hashes..].starts_with(' ') {
            flush(&mut para, &mut blocks, &mut draw);
            blocks.push(format!("{} {}", "#".repeat(hashes), draw.heading()));
        } else if let Some(prefix) = bullet_prefix(line) {
            flush(&mut para, &mut blocks, &mut draw);
            blocks.push(format!("{prefix}{}", draw.about(line.len().saturating_sub(prefix.len()))));
        } else if t.is_empty() {
            flush(&mut para, &mut blocks, &mut draw);
            blocks.push(String::new());
        } else {
            para += line.len() + 1;
        }
    }
    flush(&mut para, &mut blocks, &mut draw);

    // Blocks are sized one at a time, so the total drifts. Pull it back
    // inside the tolerance by adding to or trimming the last prose block.
    let target = est_tokens(original) as f64;
    let render = |blocks: &[String]| -> String {
        let mut s = blocks.join("\n");
        if original.ends_with('\n') && !s.ends_with('\n') {
            s.push('\n');
        }
        s
    };
    let is_prose = |b: &String| !b.is_empty() && !b.starts_with('#');
    for _ in 0..10_000 {
        let now = est_tokens(&render(&blocks)) as f64;
        if target == 0.0 || (now - target).abs() <= target * TOLERANCE / 2.0 {
            break;
        }
        if now < target {
            let extra = draw.sentence().to_string();
            match blocks.iter_mut().rev().find(|b| is_prose(b)) {
                Some(b) => {
                    b.push(' ');
                    b.push_str(&extra);
                }
                None => blocks.push(extra),
            }
        } else {
            // Where a block's own text starts, past any list marker ("1. " has a period of its own).
            let body_at = |b: &String| bullet_prefix(b).map(|p| p.len()).unwrap_or(0);
            match blocks.iter_mut().rev().find(|b| is_prose(b) && b[body_at(b)..].trim_end_matches('.').contains(". ")) {
                Some(b) => {
                    let at = body_at(b);
                    let cut = b[at..].trim_end_matches('.').rfind(". ").map(|i| at + i + 1).unwrap_or(b.len());
                    b.truncate(cut);
                }
                // Every block is one sentence: swap the longest for the corpus's shortest.
                None => {
                    let shortest = corpus.sentences.iter().min_by_key(|s| s.len()).cloned().unwrap_or_default();
                    let Some(b) = blocks.iter_mut().filter(|b| is_prose(b)).max_by_key(|b| b.len()) else { break };
                    let marker = bullet_prefix(b).map(|p| p.to_string()).unwrap_or_default();
                    if b.len() <= marker.len() + shortest.len() {
                        break;
                    }
                    *b = format!("{marker}{shortest}");
                }
            }
        }
    }
    render(&blocks)
}

/// The placebo for a skill file: frontmatter byte-identical (so it triggers
/// the same way), body replaced. For a file without frontmatter the whole
/// text is replaced.
pub fn placebo_file(text: &str, corpus: &Corpus) -> String {
    let (_, fm, body) = split_frontmatter(text);
    format!("{fm}{}", placebo_text(body, corpus))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sentences_of(text: &str) -> Vec<String> {
        crate::ablation::split_sentences(&text.lines().filter(|l| !l.trim_start().starts_with('#')).collect::<Vec<_>>().join(" ")).into_iter().map(|s| s.trim().trim_start_matches(['-', '*', ' ']).to_lowercase()).filter(|s| s.len() > 12).collect()
    }

    #[test]
    fn matches_length_and_shape() {
        let original = "# Title\n\nIntro paragraph that runs on for a little while and then stops.\n\n## Rules\n\n- First rule, stated at some length so there is something to match.\n- Second rule.\n1. Numbered item with a few words.\n\n```sh\ncargo test --all\n```\n\nClosing paragraph.\n";
        let corpus = Corpus::builtin();
        let p = placebo_text(original, &corpus);
        let (a, b) = (est_tokens(original) as f64, est_tokens(&p) as f64);
        assert!((a - b).abs() <= a * TOLERANCE, "{a} vs {b}\n{p}");
        let shape = |t: &str| (t.lines().filter(|l| l.starts_with("# ")).count(), t.lines().filter(|l| l.starts_with("## ")).count(), t.lines().filter(|l| l.starts_with("- ")).count(), t.lines().filter(|l| l.starts_with("1. ")).count());
        assert_eq!(shape(&p), shape(original));
        assert!(!p.contains("cargo") && !p.contains("```"));
        assert!(p.ends_with('\n'));
        // Deterministic: the same input gives the same placebo.
        assert_eq!(p, placebo_text(original, &corpus));
    }

    #[test]
    fn example_rfw_skill_placebo() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/overlays/rfw-v1.1.0/.claude/skills/reader-first-writing/SKILL.md");
        let text = std::fs::read_to_string(path).unwrap();
        let placebo = placebo_file(&text, &Corpus::builtin());
        let (_, fm, body) = split_frontmatter(&text);
        let (_, pfm, pbody) = split_frontmatter(&placebo);
        assert_eq!(fm, pfm, "frontmatter must be byte-identical");
        let (a, b) = (est_tokens(body) as f64, est_tokens(pbody) as f64);
        assert!((a - b).abs() <= a * TOLERANCE, "body tokens {a} vs placebo {b}");
        let original: std::collections::HashSet<String> = sentences_of(body).into_iter().collect();
        let shared: Vec<String> = sentences_of(pbody).into_iter().filter(|s| original.contains(s)).collect();
        assert!(shared.is_empty(), "placebo shares sentences with the original: {shared:?}");
    }

    /// The corpus must not move the mock runner (which reads `mock-effect:`
    /// and `mock-trigger:` lines) and must stay clear of words that steer
    /// a real agent's output.
    #[test]
    fn corpus_is_inert() {
        let c = Corpus::builtin();
        assert!(c.sentences.len() >= 30 && c.headings.len() >= 10);
        let all = format!("{}\n{}", c.sentences.join("\n"), c.headings.join("\n")).to_lowercase();
        for banned in ["mock-", "quality=", "short", "concise", "brief", "careful", "always", "never", "must", "should", "test", "check", "verify", "plan", "ask", "first", "before", "avoid", "prefer", "ensure", "impact", "hedg", "plain", "lead with"] {
            assert!(!all.contains(banned), "corpus contains `{banned}`");
        }
        let placebo = placebo_text("# Lead with impact\n\nmock-effect: 0.25\n\nSome text here that is long enough to be replaced by a sentence or two.\n", &c);
        assert!(!placebo.contains("mock-effect") && !placebo.contains("Lead with impact"));
        // A user corpus without sections is one sentence per line.
        let mine = Corpus::parse("Water is wet.\nThe sky is up.\n");
        assert_eq!(mine.sentences.len(), 2);
        assert!(!mine.headings.is_empty());
    }
}
