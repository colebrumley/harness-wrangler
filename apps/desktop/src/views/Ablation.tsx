import { useState } from "react";
import type { Ctx } from "../App";
import { Report, call, fmtDelta, fmtValue, info, remember } from "../api";
import { Empty, ErrorNote, Forest, Panel, VerdictMark, equivalenceBand } from "../components";
import { useLoad } from "../hooks";

const WORD: Record<string, string> = { carries: "carries the effect", hurts: "hurts", no_effect: "no effect", inconclusive: "inconclusive", no_headroom: "no headroom", no_data: "no data" };

export default function Ablation({ ctx }: { ctx: Ctx }) {
  const id = ctx.launchId;
  const launch = ctx.launches.find((l) => l.id === id);
  const q = useLoad<Report>(id ? "report" : null, { launch_id: id }, [launch?.runs_done, launch?.status]);
  const [creating, setCreating] = useState(false);
  const r = q.data;
  if (r) remember(r.metrics);
  const ab = r?.ablation;
  const m = ab ? info(ab.metric) : null;
  return (
    <div className="content">
      <div className="page">
        <div className="row spread top wrap">
          <div className="stack tight">
            <h1>Ablation</h1>
            <p className="muted" style={{ maxWidth: "78ch" }}>Which sections of a skill or CLAUDE.md carry its effect. Each section is removed in turn; its contribution is the full file's score minus the score without it.</p>
          </div>
          <button className="btn primary" onClick={() => setCreating((v) => !v)}>{creating ? "Close" : "New ablation"}</button>
        </div>
        {creating && <NewAblation ctx={ctx} />}
        <ErrorNote error={q.error} />
        {r && !ab && !creating && (
          <Empty title="This launch is not an ablation">
            Launch #{r.launch_id} ({r.experiment}) compares whole variants. Pick an ablation launch above, or generate one from a skill.
          </Empty>
        )}
        {r && ab && m && (
          <>
            <Panel title={ab.source} sub={`${ab.kind === "skill" ? "Skill" : "CLAUDE.md"}, measured on ${m.label.toLowerCase()} in launch #${r.launch_id}.`}>
              <div className="stack">
                <Forest
                  metric={m}
                  labelHead="Whole file"
                  band={equivalenceBand(m, ab.full_effect?.base_mean ?? 0)}
                  rows={[
                    { key: "full", label: "Full file", sub: "against no file at all", effect: ab.full_effect },
                    ...(ab.description_only_effect ? [
                      { key: "desc", label: "Description only", sub: "frontmatter with the body removed, against no file", effect: ab.description_only_effect },
                      { key: "body", label: "What the body adds", sub: "full file against description only", effect: ab.body_effect },
                    ] : []),
                  ]}
                />
                {ab.skill_invoked_rate !== null && ab.skill_invoked_rate !== undefined && (
                  <div className={`note ${ab.skill_invoked_rate < 0.5 ? "warn" : ""}`}>
                    The skill was invoked in {Math.round(ab.skill_invoked_rate * 100)}% of full-file runs.
                    {ab.skill_invoked_rate < 0.5 ? " Sections cannot show an effect in runs where the skill never loads; fix triggering first." : ""}
                  </div>
                )}
              </div>
            </Panel>
            <Panel title="Section contributions" sub="Right of zero: the section helps. Left: the file scores better without it.">
              <Forest
                metric={m}
                labelHead="Section"
                band={equivalenceBand(m, ab.full_effect?.treat_mean ?? 0)}
                rows={ab.sections.map((s: any) => ({
                  key: String(s.index),
                  label: `${s.index + 1}. ${s.title}`,
                  sub: `lines ${s.lines[0]}–${s.lines[1]}, about ${s.est_tokens} tokens`,
                  effect: s.effect,
                  word: WORD[s.attribution],
                }))}
              />
            </Panel>
            <Panel title="What each section costs" sub="Full file minus the file without the section, per run." flush>
              <table className="grid">
                <thead><tr><th>Section</th><th>Attribution</th><th className="num">Size</th><th className="num">Tokens per run</th><th className="num">Cost per run</th><th className="num">{m.label} without it</th></tr></thead>
                <tbody>
                  {ab.sections.map((s: any) => (
                    <tr key={s.index}>
                      <td><b>{s.index + 1}. {s.title}</b></td>
                      <td>{s.effect ? <VerdictMark verdict={s.effect.verdict} word={WORD[s.attribution]} /> : <span className="faint">no data</span>}</td>
                      <td className="num">~{s.est_tokens} tok</td>
                      <td className="num">{s.tokens_effect ? fmtDelta(info("total_tokens"), s.tokens_effect.effect) : "–"}</td>
                      <td className="num">{s.cost_effect ? fmtDelta(info("cost_usd"), s.cost_effect.effect) : "–"}</td>
                      <td className="num">{s.effect ? fmtValue(m, s.effect.base_mean) : "–"}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </Panel>
            <div><a className="btn" style={{ textDecoration: "none" }} href={`#/advice/${r.launch_id}`}>See what to change</a></div>
          </>
        )}
      </div>
    </div>
  );
}

function NewAblation({ ctx }: { ctx: Ctx }) {
  const overlays = useLoad<string[]>("overlays_list");
  const [source, setSource] = useState("");
  const [depth, setDepth] = useState(2);
  const [f, setF] = useState({ name: "", tasks: "tasks/**/*.toml", runner: "claude-code", model: "claude-sonnet-5-5", judge_model: "claude-opus-5-5", rubric: "", reps: 5, description_only: true });
  const [error, setError] = useState<string | null>(null);
  const preview = useLoad<any>(source ? "ablation_preview" : null, { source, depth }, []);
  const candidates = (overlays.data ?? []).filter((o) => !o.includes("/ablation/") && !o.endsWith("/none"));
  const set = (k: string, v: any) => setF((x) => ({ ...x, [k]: v }));
  const generate = async () => {
    setError(null);
    try {
      const plan = await call("ablation_generate", {
        source, depth, name: f.name || undefined, description_only: f.description_only && preview.data?.file?.endsWith("SKILL.md"),
        tasks: f.tasks.split("\n").map((s) => s.trim()).filter(Boolean), runs_per_cell: Number(f.reps) || 5,
        runner: f.runner, model: f.model || undefined, judge_model: f.judge_model || undefined, rubric: f.rubric || undefined,
      });
      ctx.go(`/experiments/${encodeURIComponent(plan.experiment)}`);
    } catch (e: any) { setError(String(e.message ?? e)); }
  };
  const sections: any[] = (preview.data?.sections ?? []).filter((s: any) => s.has_body);
  return (
    <Panel title="New ablation" sub="Writes the overlays and an experiment into the project. Nothing runs until you launch it.">
      <div className="stack">
        <div className="form-grid">
          <label className="field" style={{ gridColumn: "span 2" }}>Skill or CLAUDE.md to ablate
            <select value={source} onChange={(e) => setSource(e.target.value)}>
              <option value="">Choose an overlay…</option>
              {candidates.map((o) => <option key={o} value={o}>{o}</option>)}
            </select>
          </label>
          <label className="field">Split at heading depth
            <select value={depth} onChange={(e) => setDepth(Number(e.target.value))}>{[1, 2, 3, 4].map((d) => <option key={d} value={d}>{"#".repeat(d)} and above</option>)}</select>
          </label>
          <label className="field">Runs per cell<input type="number" min={1} value={f.reps} onChange={(e) => set("reps", e.target.value)} /></label>
          <label className="field">Runner
            <select value={f.runner} onChange={(e) => set("runner", e.target.value)}><option value="claude-code">Claude Code</option><option value="codex">Codex</option><option value="mock">Mock (offline)</option></select>
          </label>
          <label className="field">Model<input type="text" value={f.model} onChange={(e) => set("model", e.target.value)} /></label>
          <label className="field">Judge model<input type="text" value={f.judge_model} onChange={(e) => set("judge_model", e.target.value)} placeholder="none" /></label>
          <label className="field">Rubric file<input type="text" value={f.rubric} onChange={(e) => set("rubric", e.target.value)} placeholder="optional" /></label>
          <label className="field">Experiment name<input type="text" value={f.name} onChange={(e) => set("name", e.target.value)} placeholder="<skill>-ablation" /></label>
        </div>
        <label className="field">Task patterns, one per line<textarea rows={2} value={f.tasks} onChange={(e) => set("tasks", e.target.value)} /></label>
        <ErrorNote error={preview.error ?? error} />
        {preview.data && (
          <>
            <p className="muted">
              <code>{preview.data.file}</code> splits into {sections.length} sections with content (about {preview.data.est_tokens} tokens in all).
              That makes {sections.length + 2 + (f.description_only && preview.data.file.endsWith("SKILL.md") ? 1 : 0)} variants: none, full, and one without each section.
            </p>
            <table className="grid">
              <tbody>{sections.map((s: any) => <tr key={s.index}><td style={{ width: 40 }} className="faint">{s.index + 1}</td><td>{s.title}</td><td className="num muted">lines {s.start_line}–{s.end_line}</td><td className="num muted">~{s.est_tokens} tokens</td></tr>)}</tbody>
            </table>
            {preview.data.file.endsWith("SKILL.md") && <label className="check"><input type="checkbox" checked={f.description_only} onChange={(e) => set("description_only", e.target.checked)} /> Also test the description alone, to separate triggering from content</label>}
          </>
        )}
        <div><button className="btn primary" disabled={!source || sections.length < 2} onClick={generate}>Create ablation experiment</button></div>
      </div>
    </Panel>
  );
}
