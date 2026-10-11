//! SQLite store for launches, runs, metrics, grades, judgments, annotations
//! and suite baselines. Large blobs live on disk under the runs directory and
//! are referenced by `runs.dir`.

use crate::model::{Experiment, Task, Variant};
use anyhow::{anyhow, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Mutex;

pub struct Store {
    conn: Mutex<Connection>,
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS launches (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  experiment TEXT NOT NULL,
  def_json TEXT NOT NULL,
  def_hash TEXT NOT NULL,
  tasks_json TEXT NOT NULL,
  variants_json TEXT NOT NULL,
  status TEXT NOT NULL,
  phase TEXT NOT NULL DEFAULT '',
  created_at TEXT NOT NULL,
  finished_at TEXT,
  suite TEXT,
  note TEXT NOT NULL DEFAULT ''
);
CREATE TABLE IF NOT EXISTS runs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  launch_id INTEGER NOT NULL REFERENCES launches(id) ON DELETE CASCADE,
  task_id TEXT NOT NULL,
  variant_key TEXT NOT NULL,
  rep INTEGER NOT NULL,
  status TEXT NOT NULL,
  started_at TEXT,
  ended_at TEXT,
  exit_code INTEGER,
  wall_ms INTEGER,
  dir TEXT NOT NULL DEFAULT '',
  final_text TEXT NOT NULL DEFAULT '',
  output_text TEXT NOT NULL DEFAULT '',
  error TEXT NOT NULL DEFAULT '',
  events_json TEXT NOT NULL DEFAULT '[]',
  skills_json TEXT NOT NULL DEFAULT '[]',
  model TEXT NOT NULL DEFAULT '',
  UNIQUE(launch_id, task_id, variant_key, rep)
);
CREATE INDEX IF NOT EXISTS runs_launch ON runs(launch_id);
CREATE TABLE IF NOT EXISTS metrics (
  run_id INTEGER NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  value REAL NOT NULL,
  PRIMARY KEY (run_id, name)
);
CREATE TABLE IF NOT EXISTS grades (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  run_id INTEGER NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
  grader TEXT NOT NULL,
  tier TEXT NOT NULL,
  passed INTEGER,
  score REAL,
  detail_json TEXT NOT NULL DEFAULT '{}',
  UNIQUE(run_id, grader)
);
CREATE TABLE IF NOT EXISTS judgments (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  launch_id INTEGER NOT NULL REFERENCES launches(id) ON DELETE CASCADE,
  task_id TEXT NOT NULL,
  run_a INTEGER NOT NULL,
  run_b INTEGER NOT NULL,
  source TEXT NOT NULL,
  winner TEXT NOT NULL,
  reason TEXT NOT NULL DEFAULT '',
  consistent INTEGER,
  detail_json TEXT NOT NULL DEFAULT '{}',
  created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS judgments_launch ON judgments(launch_id);
CREATE TABLE IF NOT EXISTS annotations (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  run_id INTEGER NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
  kind TEXT NOT NULL,
  target TEXT NOT NULL,
  line_start INTEGER NOT NULL,
  line_end INTEGER NOT NULL,
  col_start INTEGER,
  col_end INTEGER,
  quote TEXT NOT NULL DEFAULT '',
  body TEXT NOT NULL DEFAULT '',
  created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS annotations_run ON annotations(run_id);
CREATE TABLE IF NOT EXISTS baselines (
  suite TEXT NOT NULL,
  model TEXT NOT NULL,
  experiment TEXT NOT NULL,
  launch_id INTEGER NOT NULL,
  created_at TEXT NOT NULL,
  PRIMARY KEY (suite, model, experiment)
);
"#;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Launch {
    pub id: i64,
    pub experiment: String,
    pub def: Experiment,
    pub def_hash: String,
    pub tasks: Vec<Task>,
    pub variants: Vec<Variant>,
    pub status: String,
    pub phase: String,
    pub created_at: String,
    pub finished_at: Option<String>,
    pub suite: Option<String>,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchSummary {
    pub id: i64,
    pub experiment: String,
    pub status: String,
    pub phase: String,
    pub created_at: String,
    pub finished_at: Option<String>,
    pub suite: Option<String>,
    pub note: String,
    pub runs_total: i64,
    pub runs_done: i64,
    pub runs_failed: i64,
    pub cost_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRow {
    pub id: i64,
    pub launch_id: i64,
    pub task_id: String,
    pub variant_key: String,
    pub rep: u32,
    /// pending | running | done | failed | timeout | cancelled
    pub status: String,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub exit_code: Option<i32>,
    pub wall_ms: Option<i64>,
    pub dir: String,
    pub final_text: String,
    pub output_text: String,
    pub error: String,
    pub events: serde_json::Value,
    pub skills: Vec<String>,
    pub model: String,
}

impl RunRow {
    /// A run whose agent finished and produced gradable output.
    pub fn is_complete(&self) -> bool {
        self.status == "done"
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Grade {
    pub run_id: i64,
    pub grader: String,
    /// deterministic | judge
    pub tier: String,
    pub passed: Option<bool>,
    pub score: Option<f64>,
    pub detail: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Judgment {
    pub id: i64,
    pub launch_id: i64,
    pub task_id: String,
    pub run_a: i64,
    pub run_b: i64,
    /// judge | human
    pub source: String,
    /// a | b | tie | both_bad | skip
    pub winner: String,
    pub reason: String,
    /// Judge only: did both presentation orders agree?
    pub consistent: Option<bool>,
    pub detail: serde_json::Value,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Annotation {
    #[serde(default)]
    pub id: i64,
    pub run_id: i64,
    /// comment | emoji
    pub kind: String,
    /// output | diff
    pub target: String,
    pub line_start: i64,
    pub line_end: i64,
    #[serde(default)]
    pub col_start: Option<i64>,
    #[serde(default)]
    pub col_end: Option<i64>,
    /// The annotated text, stored so agents can read it without the artifact.
    #[serde(default)]
    pub quote: String,
    /// Comment text, or the emoji.
    pub body: String,
    #[serde(default)]
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Baseline {
    pub suite: String,
    pub model: String,
    pub experiment: String,
    pub launch_id: i64,
    pub created_at: String,
}

pub fn now() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

impl Store {
    pub fn open(path: &Path) -> Result<Store> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        conn.busy_timeout(std::time::Duration::from_secs(30))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Store { conn: Mutex::new(conn) })
    }

    pub fn in_memory() -> Result<Store> {
        let conn = Connection::open_in_memory()?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Store { conn: Mutex::new(conn) })
    }

    fn c(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    // ------------------------------------------------------------ launches

    pub fn create_launch(
        &self,
        exp: &Experiment,
        tasks: &[Task],
        variants: &[Variant],
        suite: Option<&str>,
        note: &str,
    ) -> Result<i64> {
        let def_json = serde_json::to_string(exp)?;
        let hash = crate::util::sha256_hex(def_json.as_bytes());
        let mut c = self.c();
        let tx = c.transaction()?;
        tx.execute(
            "INSERT INTO launches (experiment, def_json, def_hash, tasks_json, variants_json, status, phase, created_at, suite, note)
             VALUES (?1, ?2, ?3, ?4, ?5, 'pending', '', ?6, ?7, ?8)",
            params![exp.name, def_json, &hash[..12], serde_json::to_string(tasks)?, serde_json::to_string(variants)?, now(), suite, note],
        )?;
        let id = tx.last_insert_rowid();
        for rep in 0..exp.runs_per_cell {
            for t in tasks {
                for v in variants {
                    tx.execute(
                        "INSERT INTO runs (launch_id, task_id, variant_key, rep, status) VALUES (?1, ?2, ?3, ?4, 'pending')",
                        params![id, t.id, v.key, rep],
                    )?;
                }
            }
        }
        tx.commit()?;
        Ok(id)
    }

    pub fn launch(&self, id: i64) -> Result<Launch> {
        let c = self.c();
        c.query_row(
            "SELECT id, experiment, def_json, def_hash, tasks_json, variants_json, status, phase, created_at, finished_at, suite, note FROM launches WHERE id = ?1",
            [id],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, String>(7)?,
                    r.get::<_, String>(8)?,
                    r.get::<_, Option<String>>(9)?,
                    r.get::<_, Option<String>>(10)?,
                    r.get::<_, String>(11)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| anyhow!("launch {id} not found"))
        .and_then(|t| {
            Ok(Launch {
                id: t.0,
                experiment: t.1,
                def: serde_json::from_str(&t.2)?,
                def_hash: t.3,
                tasks: serde_json::from_str(&t.4)?,
                variants: serde_json::from_str(&t.5)?,
                status: t.6,
                phase: t.7,
                created_at: t.8,
                finished_at: t.9,
                suite: t.10,
                note: t.11,
            })
        })
    }

    pub fn latest_launch(&self, experiment: &str) -> Result<Option<i64>> {
        Ok(self
            .c()
            .query_row("SELECT id FROM launches WHERE experiment = ?1 ORDER BY id DESC LIMIT 1", [experiment], |r| r.get(0))
            .optional()?)
    }

    /// Resolve a launch reference: a numeric id, or an experiment name
    /// (meaning its most recent launch).
    pub fn resolve_launch(&self, reference: &str) -> Result<i64> {
        if let Ok(id) = reference.trim_start_matches('#').parse::<i64>() {
            self.launch(id)?;
            return Ok(id);
        }
        self.latest_launch(reference)?.ok_or_else(|| anyhow!("no launches found for experiment `{reference}`"))
    }

    pub fn list_launches(&self, experiment: Option<&str>) -> Result<Vec<LaunchSummary>> {
        let c = self.c();
        let mut stmt = c.prepare(
            "SELECT l.id, l.experiment, l.status, l.phase, l.created_at, l.finished_at, l.suite, l.note,
               (SELECT COUNT(*) FROM runs r WHERE r.launch_id = l.id),
               (SELECT COUNT(*) FROM runs r WHERE r.launch_id = l.id AND r.status = 'done'),
               (SELECT COUNT(*) FROM runs r WHERE r.launch_id = l.id AND r.status IN ('failed','timeout')),
               (SELECT COALESCE(SUM(m.value),0) FROM metrics m JOIN runs r ON r.id = m.run_id WHERE r.launch_id = l.id AND m.name IN ('cost_usd','judge_cost_usd'))
             FROM launches l WHERE (?1 IS NULL OR l.experiment = ?1) ORDER BY l.id DESC",
        )?;
        let rows = stmt.query_map([experiment], |r| {
            Ok(LaunchSummary {
                id: r.get(0)?,
                experiment: r.get(1)?,
                status: r.get(2)?,
                phase: r.get(3)?,
                created_at: r.get(4)?,
                finished_at: r.get(5)?,
                suite: r.get(6)?,
                note: r.get(7)?,
                runs_total: r.get(8)?,
                runs_done: r.get(9)?,
                runs_failed: r.get(10)?,
                cost_usd: r.get(11)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn set_launch_status(&self, id: i64, status: &str, phase: &str) -> Result<()> {
        let finished = matches!(status, "done" | "failed" | "cancelled").then(now);
        self.c().execute(
            "UPDATE launches SET status = ?2, phase = ?3, finished_at = COALESCE(?4, finished_at) WHERE id = ?1",
            params![id, status, phase, finished],
        )?;
        Ok(())
    }

    pub fn delete_launch(&self, id: i64) -> Result<()> {
        let c = self.c();
        c.execute("DELETE FROM baselines WHERE launch_id = ?1", [id])?;
        c.execute("DELETE FROM launches WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Mark launches left `running` by a dead process as interrupted.
    pub fn mark_stale(&self, live: &[i64]) -> Result<()> {
        let c = self.c();
        let ids: Vec<i64> = {
            let mut s = c.prepare("SELECT id FROM launches WHERE status = 'running'")?;
            let v = s.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<Vec<i64>>>()?;
            v
        };
        for id in ids.into_iter().filter(|i| !live.contains(i)) {
            c.execute("UPDATE runs SET status = 'pending' WHERE launch_id = ?1 AND status = 'running'", [id])?;
            c.execute("UPDATE launches SET status = 'interrupted', phase = '' WHERE id = ?1", [id])?;
        }
        Ok(())
    }

    // ---------------------------------------------------------------- runs

    fn row_to_run(r: &rusqlite::Row) -> rusqlite::Result<RunRow> {
        let events: String = r.get(14)?;
        let skills: String = r.get(15)?;
        Ok(RunRow {
            id: r.get(0)?,
            launch_id: r.get(1)?,
            task_id: r.get(2)?,
            variant_key: r.get(3)?,
            rep: r.get(4)?,
            status: r.get(5)?,
            started_at: r.get(6)?,
            ended_at: r.get(7)?,
            exit_code: r.get(8)?,
            wall_ms: r.get(9)?,
            dir: r.get(10)?,
            final_text: r.get(11)?,
            output_text: r.get(12)?,
            error: r.get(13)?,
            events: serde_json::from_str(&events).unwrap_or(serde_json::Value::Null),
            skills: serde_json::from_str(&skills).unwrap_or_default(),
            model: r.get(16)?,
        })
    }

    const RUN_COLS: &'static str = "id, launch_id, task_id, variant_key, rep, status, started_at, ended_at, exit_code, wall_ms, dir, final_text, output_text, error, events_json, skills_json, model";

    pub fn runs(&self, launch_id: i64) -> Result<Vec<RunRow>> {
        let c = self.c();
        let mut stmt = c.prepare(&format!("SELECT {} FROM runs WHERE launch_id = ?1 ORDER BY id", Self::RUN_COLS))?;
        let rows = stmt.query_map([launch_id], Self::row_to_run)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn run(&self, id: i64) -> Result<RunRow> {
        self.c()
            .query_row(&format!("SELECT {} FROM runs WHERE id = ?1", Self::RUN_COLS), [id], Self::row_to_run)
            .optional()?
            .ok_or_else(|| anyhow!("run {id} not found"))
    }

    pub fn mark_run_started(&self, id: i64, dir: &str) -> Result<()> {
        self.c().execute(
            "UPDATE runs SET status = 'running', started_at = ?2, dir = ?3, error = '' WHERE id = ?1",
            params![id, now(), dir],
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn finish_run(
        &self,
        id: i64,
        status: &str,
        exit_code: Option<i32>,
        wall_ms: i64,
        final_text: &str,
        output_text: &str,
        error: &str,
        events: &serde_json::Value,
        skills: &[String],
        model: &str,
        metrics: &BTreeMap<String, f64>,
    ) -> Result<()> {
        let mut c = self.c();
        let tx = c.transaction()?;
        tx.execute(
            "UPDATE runs SET status = ?2, ended_at = ?3, exit_code = ?4, wall_ms = ?5, final_text = ?6, output_text = ?7, error = ?8, events_json = ?9, skills_json = ?10, model = ?11 WHERE id = ?1",
            params![id, status, now(), exit_code, wall_ms, final_text, output_text, error, events.to_string(), serde_json::to_string(skills)?, model],
        )?;
        tx.execute("DELETE FROM metrics WHERE run_id = ?1", [id])?;
        tx.execute("DELETE FROM grades WHERE run_id = ?1", [id])?;
        for (k, v) in metrics {
            if v.is_finite() {
                tx.execute("INSERT OR REPLACE INTO metrics (run_id, name, value) VALUES (?1, ?2, ?3)", params![id, k, v])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn set_run_status(&self, id: i64, status: &str, error: &str) -> Result<()> {
        self.c().execute("UPDATE runs SET status = ?2, error = ?3 WHERE id = ?1", params![id, status, error])?;
        Ok(())
    }

    /// Reset unfinished and failed runs so a resume picks them up.
    pub fn reset_incomplete(&self, launch_id: i64, include_failed: bool) -> Result<usize> {
        let sql = if include_failed {
            "UPDATE runs SET status = 'pending' WHERE launch_id = ?1 AND status != 'done'"
        } else {
            "UPDATE runs SET status = 'pending' WHERE launch_id = ?1 AND status IN ('running','cancelled')"
        };
        Ok(self.c().execute(sql, [launch_id])?)
    }

    pub fn set_metric(&self, run_id: i64, name: &str, value: f64) -> Result<()> {
        if value.is_finite() {
            self.c().execute("INSERT OR REPLACE INTO metrics (run_id, name, value) VALUES (?1, ?2, ?3)", params![run_id, name, value])?;
        }
        Ok(())
    }

    pub fn metrics_for_launch(&self, launch_id: i64) -> Result<HashMap<i64, BTreeMap<String, f64>>> {
        let c = self.c();
        let mut stmt = c.prepare("SELECT m.run_id, m.name, m.value FROM metrics m JOIN runs r ON r.id = m.run_id WHERE r.launch_id = ?1")?;
        let mut out: HashMap<i64, BTreeMap<String, f64>> = HashMap::new();
        for row in stmt.query_map([launch_id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, f64>(2)?)))? {
            let (id, n, v) = row?;
            out.entry(id).or_default().insert(n, v);
        }
        Ok(out)
    }

    pub fn metrics_for_run(&self, run_id: i64) -> Result<BTreeMap<String, f64>> {
        let c = self.c();
        let mut stmt = c.prepare("SELECT name, value FROM metrics WHERE run_id = ?1")?;
        let rows = stmt.query_map([run_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<BTreeMap<_, _>>>()?)
    }

    // -------------------------------------------------------------- grades

    pub fn put_grade(&self, g: &Grade) -> Result<()> {
        self.c().execute(
            "INSERT INTO grades (run_id, grader, tier, passed, score, detail_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(run_id, grader) DO UPDATE SET tier = ?3, passed = ?4, score = ?5, detail_json = ?6",
            params![g.run_id, g.grader, g.tier, g.passed, g.score, g.detail.to_string()],
        )?;
        Ok(())
    }

    fn row_to_grade(r: &rusqlite::Row) -> rusqlite::Result<Grade> {
        let d: String = r.get(5)?;
        Ok(Grade {
            run_id: r.get(0)?,
            grader: r.get(1)?,
            tier: r.get(2)?,
            passed: r.get(3)?,
            score: r.get(4)?,
            detail: serde_json::from_str(&d).unwrap_or(serde_json::Value::Null),
        })
    }

    pub fn grades_for_run(&self, run_id: i64) -> Result<Vec<Grade>> {
        let c = self.c();
        let mut stmt = c.prepare("SELECT run_id, grader, tier, passed, score, detail_json FROM grades WHERE run_id = ?1 ORDER BY id")?;
        let rows = stmt.query_map([run_id], Self::row_to_grade)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn grades_for_launch(&self, launch_id: i64) -> Result<Vec<Grade>> {
        let c = self.c();
        let mut stmt = c.prepare(
            "SELECT g.run_id, g.grader, g.tier, g.passed, g.score, g.detail_json FROM grades g JOIN runs r ON r.id = g.run_id WHERE r.launch_id = ?1 ORDER BY g.id",
        )?;
        let rows = stmt.query_map([launch_id], Self::row_to_grade)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    // ----------------------------------------------------------- judgments

    pub fn add_judgment(&self, j: &Judgment) -> Result<i64> {
        let c = self.c();
        c.execute(
            "INSERT INTO judgments (launch_id, task_id, run_a, run_b, source, winner, reason, consistent, detail_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![j.launch_id, j.task_id, j.run_a, j.run_b, j.source, j.winner, j.reason, j.consistent, j.detail.to_string(), now()],
        )?;
        Ok(c.last_insert_rowid())
    }

    pub fn judgments(&self, launch_id: i64, source: Option<&str>) -> Result<Vec<Judgment>> {
        let c = self.c();
        let mut stmt = c.prepare(
            "SELECT id, launch_id, task_id, run_a, run_b, source, winner, reason, consistent, detail_json, created_at
             FROM judgments WHERE launch_id = ?1 AND (?2 IS NULL OR source = ?2) ORDER BY id",
        )?;
        let rows = stmt.query_map(params![launch_id, source], |r| {
            let d: String = r.get(9)?;
            Ok(Judgment {
                id: r.get(0)?,
                launch_id: r.get(1)?,
                task_id: r.get(2)?,
                run_a: r.get(3)?,
                run_b: r.get(4)?,
                source: r.get(5)?,
                winner: r.get(6)?,
                reason: r.get(7)?,
                consistent: r.get(8)?,
                detail: serde_json::from_str(&d).unwrap_or(serde_json::Value::Null),
                created_at: r.get(10)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn delete_judgment(&self, id: i64) -> Result<()> {
        self.c().execute("DELETE FROM judgments WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn clear_judge_judgments(&self, launch_id: i64) -> Result<()> {
        self.c().execute("DELETE FROM judgments WHERE launch_id = ?1 AND source = 'judge'", [launch_id])?;
        Ok(())
    }

    // --------------------------------------------------------- annotations

    pub fn add_annotation(&self, a: &Annotation) -> Result<i64> {
        let c = self.c();
        c.execute(
            "INSERT INTO annotations (run_id, kind, target, line_start, line_end, col_start, col_end, quote, body, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![a.run_id, a.kind, a.target, a.line_start, a.line_end, a.col_start, a.col_end, a.quote, a.body, now()],
        )?;
        Ok(c.last_insert_rowid())
    }

    pub fn delete_annotation(&self, id: i64) -> Result<()> {
        self.c().execute("DELETE FROM annotations WHERE id = ?1", [id])?;
        Ok(())
    }

    fn row_to_annotation(r: &rusqlite::Row) -> rusqlite::Result<Annotation> {
        Ok(Annotation {
            id: r.get(0)?,
            run_id: r.get(1)?,
            kind: r.get(2)?,
            target: r.get(3)?,
            line_start: r.get(4)?,
            line_end: r.get(5)?,
            col_start: r.get(6)?,
            col_end: r.get(7)?,
            quote: r.get(8)?,
            body: r.get(9)?,
            created_at: r.get(10)?,
        })
    }

    const ANN_COLS: &'static str = "a.id, a.run_id, a.kind, a.target, a.line_start, a.line_end, a.col_start, a.col_end, a.quote, a.body, a.created_at";

    pub fn annotations_for_run(&self, run_id: i64) -> Result<Vec<Annotation>> {
        let c = self.c();
        let mut stmt = c.prepare(&format!("SELECT {} FROM annotations a WHERE a.run_id = ?1 ORDER BY a.line_start, a.id", Self::ANN_COLS))?;
        let rows = stmt.query_map([run_id], Self::row_to_annotation)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn annotations_for_launch(&self, launch_id: i64) -> Result<Vec<Annotation>> {
        let c = self.c();
        let mut stmt = c.prepare(&format!(
            "SELECT {} FROM annotations a JOIN runs r ON r.id = a.run_id WHERE r.launch_id = ?1 ORDER BY a.run_id, a.line_start, a.id",
            Self::ANN_COLS
        ))?;
        let rows = stmt.query_map([launch_id], Self::row_to_annotation)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    // ----------------------------------------------------------- baselines

    pub fn set_baseline(&self, suite: &str, model: &str, experiment: &str, launch_id: i64) -> Result<()> {
        self.c().execute(
            "INSERT OR REPLACE INTO baselines (suite, model, experiment, launch_id, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![suite, model, experiment, launch_id, now()],
        )?;
        Ok(())
    }

    pub fn baselines(&self, suite: &str) -> Result<Vec<Baseline>> {
        let c = self.c();
        let mut stmt = c.prepare("SELECT suite, model, experiment, launch_id, created_at FROM baselines WHERE suite = ?1 ORDER BY created_at, model")?;
        let rows = stmt.query_map([suite], |r| {
            Ok(Baseline { suite: r.get(0)?, model: r.get(1)?, experiment: r.get(2)?, launch_id: r.get(3)?, created_at: r.get(4)? })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}
