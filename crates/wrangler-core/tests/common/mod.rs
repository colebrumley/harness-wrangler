//! A throwaway project wired to the mock runner and mock judge.
#![allow(dead_code)]

use std::path::Path;
use wrangler_core::Project;

pub fn write(root: &Path, rel: &str, content: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}

pub const SKILL: &str = "---\nname: plain\ndescription: Use when writing summaries.\n---\n# Plain writing\n\nmock-effect: 0.02\n\n## Lead with impact\n\nmock-effect: 0.25\n\n## Cut hedges\n\nmock-effect: 0.0\n\n## Add flourish\n\nmock-effect: -0.12\n";

/// Mock project: a helpful skill, a harmful CLAUDE.md, three writing tasks.
pub fn project(root: &Path) -> Project {
    let project = Project::init(root).unwrap();
    write(root, "overlays/plain/.claude/skills/plain/SKILL.md", SKILL);
    write(root, "overlays/noisy/CLAUDE.md", "Be verbose.\nmock-effect: -0.2\n");
    write(root, "rubrics/plain.md", "Prefer short, direct summaries.\n");
    write(root, "fixtures/incident/REPORT.md", "Checkout failed for 12 minutes.\n");
    for id in ["alpha", "bravo", "charlie"] {
        write(
            root,
            &format!("tasks/{id}.toml"),
            &format!(
                "id = \"{id}\"\nprompt = \"Summarize REPORT.md into SUMMARY.md\"\nfixture = {{ dir = \"fixtures/incident\" }}\noutput_file = \"SUMMARY.md\"\nexpected_output = \"A short summary\"\nexpectations = [\"Leads with impact\", \"Under 150 words\", \"No hedging\"]\n\n[graders]\ndeterministic = [{{ kind = \"word_count\", path = \"SUMMARY.md\", max = 130 }}, {{ kind = \"file_exists\", path = \"SUMMARY.md\" }}]\n"
            ),
        );
    }
    write(
        root,
        "experiments/demo.toml",
        r#"name = "demo"
description = "Mock experiment"
runs_per_cell = 4

[runner]
kind = "mock"
concurrency = 4

[[factors]]
name = "overlay"
kind = "overlay"
levels = ["overlays/none", "overlays/plain", "overlays/noisy"]

[[factors]]
name = "effort"
kind = "effort"
levels = ["medium", "high"]

[tasks]
include = ["tasks/*.toml"]

[graders.default]
judge = { model = "mock", rubric = "rubrics/plain.md", pairwise = true, pairwise_mode = "baseline" }
human = { enabled = true }
"#,
    );
    project
}
