import { useEffect, useState } from "react";
import type { Ctx } from "../App";
import { call, fmtDelta, info, when } from "../api";
import { Empty, ErrorNote, Panel, StatusChip, Tabs, VerdictMark } from "../components";
import { useLoad } from "../hooks";

const VERDICT: Record<string, [string, string]> = {
  helping: ["good", "▲ helping"],
  harming: ["bad", "▼ harming"],
  neutral: ["", "● no effect"],
  inconclusive: ["warn", "○ inconclusive"],
  no_headroom: ["warn", "◌ no headroom"],
  no_data: ["", "no data"],
};
const CATEGORY: Record<string, [string, string]> = {
  harming: ["Harming", "These hurt on the new model, as they did before."],
  regressed: ["Regressed", "These got worse on the new model. Look at them first."],
  redundant: ["Now redundant", "The new model reaches the same result without the pattern."],
  inconclusive: ["Cannot tell yet", "The interval is too wide. More runs would settle it."],
  not_measured: ["Not measured", "Every task is at the bound with and without the pattern, so there was no room to show anything. Add harder tasks."],
  no_effect: ["No measurable effect", "Not helping on either model."],
  new: ["Helping", "First baseline for these."],
  earning: ["Still earning their keep", "Helping on the new model."],
};

export default function Regression({ ctx, selected }: { ctx: Ctx; selected: string | null }) {
  const list = useLoad<any[]>("suites_list", {}, [], 5000);
  const suites = list.data ?? [];
  const [creating, setCreating] = useState(false);
  useEffect(() => { if (!selected && suites.length > 0) ctx.go(`/regression/${encodeURIComponent(suites[0].suite.name)}`); }, [selected, suites.length]);
  return (
    <div className="content">
      <div className="split" style={{ maxWidth: 1400 }}>
        <div className="stack">
          <button className="btn primary" onClick={() => setCreating(true)}>New suite</button>
          <section className="panel">
            <div className="list">
              {suites.map((s) => (
                <button key={s.suite.name} className={selected === s.suite.name && !creating ? "on" : ""} onClick={() => { setCreating(false); ctx.go(`/regression/${encodeURIComponent(s.suite.name)}`); }}>
                  <b className="row spread"><span className="clip">{s.suite.name}</span>{s.running && <StatusChip status="running" />}</b>
                  <span>{s.suite.experiments.length} experiment{s.suite.experiments.length === 1 ? "" : "s"}, {s.models.length ? `${s.models.length} model baseline${s.models.length === 1 ? "" : "s"}` : "no baselines yet"}</span>
                </button>
              ))}
              {list.data && suites.length === 0 && <div className="empty">No suites yet.</div>}
            </div>
          </section>
          <ErrorNote error={list.error} />
        </div>
        <div className="stack" style={{ minWidth: 0 }}>
          {creating && <NewSuite onDone={(name) => { setCreating(false); list.reload(); if (name) ctx.go(`/regression/${encodeURIComponent(name)}`); }} />}
          {!creating && selected && <SuiteView name={selected} ctx={ctx} onChanged={list.reload} />}
          {!creating && !selected && list.data && suites.length === 0 && (
            <Empty title="Track patterns across models">
              A suite pins experiments as a regression set. Run it once per model; when a new model ships, run it again and see what regressed, what became redundant, and what still earns its keep.
            </Empty>
          )}
        </div>
      </div>
    </div>
  );
}

function NewSuite({ onDone }: { onDone: (name: string | null) => void }) {
  const exps = useLoad<any[]>("experiments_list");
  const [name, setName] = useState("");
  const [metric, setMetric] = useState("judge_score");
  const [picked, setPicked] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);
  const save = async () => {
    try { await call("suite_save", { suite: { name, description: "", primary_metric: metric, experiments: picked } }); onDone(name); }
    catch (e: any) { setError(String(e.message ?? e)); }
  };
  return (
    <Panel title="New suite" sub="Each experiment needs an overlay factor whose first level is the no-pattern baseline.">
      <div className="stack">
        <div className="form-grid">
          <label className="field">Name<input type="text" value={name} onChange={(e) => setName(e.target.value.replace(/[^\w.-]/g, "-"))} /></label>
          <label className="field">Primary metric
            <select value={metric} onChange={(e) => setMetric(e.target.value)}>
              <option value="judge_score">Judge score</option><option value="pass">Pass rate</option><option value="expectations_passed">Expectations met</option>
            </select>
          </label>
        </div>
        <div className="stack tight">
          {(exps.data ?? []).filter((e) => !e.error).map((e) => (
            <label key={e.name} className="check"><input type="checkbox" checked={picked.includes(e.name)} onChange={(ev) => setPicked((p) => ev.target.checked ? [...p, e.name] : p.filter((x) => x !== e.name))} /> <b>{e.name}</b> <span className="muted small">{e.description}</span></label>
          ))}
        </div>
        <ErrorNote error={error} />
        <div className="row"><button className="btn primary" disabled={!name || picked.length === 0} onClick={save}>Save suite</button><button className="btn" onClick={() => onDone(null)}>Cancel</button></div>
      </div>
    </Panel>
  );
}

function SuiteView({ name, ctx, onChanged }: { name: string; ctx: Ctx; onChanged: () => void }) {
  const [tab, setTab] = useState<"dashboard" | "diff">("dashboard");
  const [poll, setPoll] = useState(false);
  const q = useLoad<any>("suite_get", { name }, [], poll ? 2000 : 0);
  const [model, setModel] = useState("");
  const [error, setError] = useState<string | null>(null);
  const d = q.data;
  const running = !!d?.job?.running;
  useEffect(() => { setPoll(running); if (!running) onChanged(); }, [running]);
  useEffect(() => { setTab("dashboard"); setError(null); }, [name]);
  if (!d) return <ErrorNote error={q.error} />;
  const dash = d.dashboard;
  const m = info(dash.suite.primary_metric);
  const run = async () => {
    setError(null);
    try { await call("suite_run", { name, model: model.trim() }); setPoll(true); q.reload(); ctx.reloadLaunches(); }
    catch (e: any) { setError(String(e.message ?? e)); }
  };
  return (
    <>
      <div className="row spread top wrap">
        <div className="stack tight">
          <h1>{name}</h1>
          <p className="muted">{dash.suite.description || `Tracks ${dash.suite.experiments.join(", ")}.`} Verdicts use {m.label.toLowerCase()}.</p>
        </div>
      </div>
      <Panel title="Run on a model" sub="Reruns every experiment in the suite with this model and saves the result as its baseline.">
        <div className="stack">
          <div className="row wrap">
            <input type="text" placeholder="Model id, for example claude-opus-5-5" value={model} onChange={(e) => setModel(e.target.value)} style={{ width: 320 }} aria-label="Model id" list="known-models" />
            <datalist id="known-models">{["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5", "claude-fable-5-1", ...dash.models].map((x: string) => <option key={x} value={x} />)}</datalist>
            <button className="btn primary" disabled={!model.trim() || running} onClick={run}>{dash.models.includes(model.trim()) ? "Rerun baseline" : "Run suite"}</button>
            {running && <button className="btn danger" onClick={() => call("suite_cancel", { name })}>Cancel</button>}
          </div>
          {running && <div className="note">Running on {d.job.model}. {(d.active_launches ?? []).map((l: any) => `${l.experiment}: ${l.runs_done}/${l.runs_total} runs${l.phase && l.phase !== "runs" ? `, ${l.phase}` : ""}`).join("; ")}</div>}
          <ErrorNote error={error ?? d.job?.error} />
        </div>
      </Panel>
      {dash.models.length === 0 ? (
        <Empty title="No baselines yet">Run the suite on the model you use today. That becomes the baseline the next model is compared with.</Empty>
      ) : (
        <>
          <Tabs tabs={[["dashboard", "Dashboard"], ["diff", "New-model diff"]]} value={tab} onChange={setTab} />
          {tab === "dashboard" && (
            <Panel title="Effect of each pattern, per model" sub={`${m.label} with the pattern minus without it, 95% interval. Models are in the order they joined the suite.`} flush>
              <div className="scrollx">
                <table className="grid matrix">
                  <thead><tr><th>Pattern</th>{dash.models.map((x: string) => <th key={x} className="model">{x}</th>)}</tr></thead>
                  <tbody>
                    {dash.rows.map((row: any) => (
                      <tr key={`${row.experiment}|${row.pattern}`}>
                        <td><b>{row.pattern}</b><div className="muted small">{row.experiment}</div></td>
                        {dash.models.map((model: string) => {
                          const c = row.cells.find((x: any) => x.model === model);
                          if (!c) return <td key={model} className="faint">not run</td>;
                          const [cls, word] = VERDICT[c.verdict] ?? ["", c.verdict];
                          return (
                            <td key={model}>
                              <div className="mcell">
                                <div className="line">
                                  <span className={`chip ${cls}`}>{word}</span>
                                  {c.effect && <span><b>{fmtDelta(m, c.effect.effect)}</b> <span className="faint">[{fmtDelta(m, c.effect.ci_low)}, {fmtDelta(m, c.effect.ci_high)}]</span></span>}
                                </div>
                                <div className="line small muted">
                                  {c.change_vs_prev !== null && c.change_vs_prev !== undefined && <span className={Math.abs(c.change_vs_prev) < 1e-9 ? "" : (c.change_vs_prev > 0) === (m.direction !== "lower") ? "delta-up" : "delta-down"}>{fmtDelta(m, c.change_vs_prev)} vs {c.prev_model}</span>}
                                  {c.cost_effect && <span>cost {fmtDelta(info("cost_usd"), c.cost_effect.effect)}</span>}
                                  <a href={`#/results/${c.launch_id}`}>launch #{c.launch_id}</a>
                                </div>
                              </div>
                            </td>
                          );
                        })}
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
              {dash.missing.length > 0 && <div style={{ padding: "8px 16px" }}>{dash.missing.map((x: string) => <div key={x} className="faint small">{x}</div>)}</div>}
            </Panel>
          )}
          {tab === "diff" && <ModelDiff name={name} models={dash.models} />}
        </>
      )}
    </>
  );
}

function ModelDiff({ name, models }: { name: string; models: string[] }) {
  const [model, setModel] = useState(models[models.length - 1]);
  const idx = models.indexOf(model);
  const [prev, setPrev] = useState<string>("");
  useEffect(() => { setPrev(""); }, [model]);
  const q = useLoad<any>("suite_diff", { name, model, previous: prev || undefined }, []);
  const d = q.data;
  const cats = d ? Array.from(new Set(d.items.map((i: any) => i.category))) as string[] : [];
  return (
    <>
      <div className="row wrap">
        <label className="row small muted" style={{ gap: 6 }}>New model<select value={model} onChange={(e) => setModel(e.target.value)}>{models.map((x) => <option key={x}>{x}</option>)}</select></label>
        <label className="row small muted" style={{ gap: 6 }}>compared with
          <select value={prev} onChange={(e) => setPrev(e.target.value)}>
            <option value="">{idx > 0 ? `${models[idx - 1]} (the one before it)` : "nothing earlier"}</option>
            {models.filter((x) => x !== model).map((x) => <option key={x}>{x}</option>)}
          </select>
        </label>
      </div>
      <ErrorNote error={q.error} />
      {d && !d.prev_model && <div className="note">{model} is the first model in this suite, so there is nothing to compare with yet. Its results are listed below.</div>}
      {d && cats.map((cat) => (
        <Panel key={cat} title={CATEGORY[cat]?.[0] ?? cat} sub={CATEGORY[cat]?.[1]} flush>
          <table className="grid">
            <tbody>
              {d.items.filter((i: any) => i.category === cat).map((i: any) => (
                <tr key={`${i.experiment}|${i.pattern}`}>
                  <td style={{ width: "24%" }}><b>{i.pattern}</b><div className="muted small">{i.experiment}</div></td>
                  <td>
                    {i.summary}
                    <div className="row small muted" style={{ marginTop: 2 }}>
                      {i.new.effect && <VerdictMark verdict={i.new.effect.verdict} />}
                      <a href={`#/results/${i.new.launch_id}`}>launch #{i.new.launch_id} ({when(i.new.created_at)})</a>
                      {i.prev && <a href={`#/results/${i.prev.launch_id}`}>previous: launch #{i.prev.launch_id}</a>}
                    </div>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </Panel>
      ))}
    </>
  );
}
