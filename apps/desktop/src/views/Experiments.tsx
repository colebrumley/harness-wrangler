import { useEffect, useRef, useState } from "react";
import type { Ctx } from "../App";
import { LaunchSummary, call, usd, when } from "../api";
import { Empty, ErrorNote, Panel, Seg, StatusChip } from "../components";
import { useLoad } from "../hooks";

const FACTOR_KINDS: [string, string][] = [
  ["overlay", "Overlay (skills, CLAUDE.md, settings)"],
  ["model", "Model"],
  ["effort", "Effort"],
  ["runner", "Runner"],
  ["system_prompt_append", "System prompt append"],
  ["knob", "Runner flag or env var"],
];

export default function Experiments({ ctx, selected, sub }: { ctx: Ctx; selected: string | null; sub: string | null }) {
  const list = useLoad<any[]>("experiments_list", {}, [ctx.launches.length], 5000);
  const items = list.data ?? [];
  useEffect(() => {
    if (!selected && items.length > 0) ctx.go(`/experiments/${encodeURIComponent(items[0].name)}`);
  }, [selected, items.length]);
  // False only once the list has loaded without the selected name in it.
  const listed = !list.data || items.some((e) => e.name === selected);
  return (
    <div className="content">
      <div className="split" style={{ maxWidth: 1320 }}>
        <div className="stack">
          <div className="row">
            <button className="btn primary grow" onClick={() => ctx.go("/experiments/new")}>New experiment</button>
            <button className={`btn ${selected === "~tasks" ? "primary" : ""}`} onClick={() => ctx.go("/experiments/~tasks")}>Tasks</button>
          </div>
          <section className="panel">
            <div className="list">
              {items.map((e) => (
                <button key={e.name} className={selected === e.name ? "on" : ""} onClick={() => ctx.go(`/experiments/${encodeURIComponent(e.name)}`)}>
                  <b className="row spread">
                    <span className="clip">{e.name}</span>
                    {e.last_launch && <StatusChip status={e.last_launch.status} />}
                  </b>
                  <span>
                    {e.error ? "Has an error" : `${e.variants} variants, ${e.tasks} tasks, ${e.runs_per_cell} runs each`}
                    {e.ablation ? " (ablation)" : ""}
                  </span>
                </button>
              ))}
              {list.data && items.length === 0 && <div className="empty">No experiments in this project yet.</div>}
            </div>
          </section>
          <ErrorNote error={list.error} />
        </div>
        <div className="stack" style={{ minWidth: 0 }}>
          {selected === "~tasks" && <Tasks ctx={ctx} />}
          {selected === "new" && <Editor name={null} ctx={ctx} onSaved={(n) => { list.reload(); ctx.go(`/experiments/${encodeURIComponent(n)}`); }} />}
          {selected && selected !== "new" && selected !== "~tasks" && sub === "edit" && <Editor name={selected} listed={listed} ctx={ctx} onSaved={(n) => { list.reload(); ctx.go(`/experiments/${encodeURIComponent(n)}`); }} />}
          {selected && selected !== "new" && selected !== "~tasks" && sub !== "edit" && <Detail name={selected} listed={listed} ctx={ctx} onChanged={list.reload} />}
          {!selected && list.data && items.length === 0 && (
            <Empty title="This project is empty, which is the normal starting point">
              <p style={{ maxWidth: "62ch", margin: "0 auto 14px" }}>
                Two things make an experiment: tasks (what the agent is asked to do) and factors (what you vary between runs, such as a skill, a model or an effort level).
                Start with a task, then create the experiment.
              </p>
              <div className="row" style={{ justifyContent: "center" }}>
                <button className="btn primary" onClick={() => ctx.go("/experiments/~tasks")}>Add a task</button>
                <button className="btn" onClick={() => ctx.go("/experiments/new")}>New experiment</button>
              </div>
            </Empty>
          )}
        </div>
      </div>
    </div>
  );
}

/** Overlay paths read better as their last folder; everything else as written. */
function levelLabel(kind: string, l: any): string {
  if (typeof l !== "string") return l.label ?? l.value;
  if (!l) return "(none)";
  return kind === "overlay" ? l.replace(/\/$/, "").split("/").pop()! : l.split("\n")[0].slice(0, 48);
}

/** The route names an experiment whose file is gone: deleted, renamed, or a stale link. */
function Missing({ name, ctx }: { name: string; ctx: Ctx }) {
  const mine = ctx.launches.filter((l) => l.experiment === name);
  return (
    <Empty title={`No experiment named ${name}`}>
      <p style={{ maxWidth: "62ch", margin: "0 auto 14px" }}>
        Its file is not in this project's experiments folder. It may have been deleted or renamed.
        {mine.length > 0 && ` The results of its ${mine.length === 1 ? "launch" : `${mine.length} launches`} are still here.`}
      </p>
      <div className="row" style={{ justifyContent: "center" }}>
        {mine.length > 0 && <button className="btn primary" onClick={() => ctx.go(`/results/${mine[0].id}`)}>See its results</button>}
        <button className="btn" onClick={() => ctx.go("/experiments")}>Back to experiments</button>
      </div>
    </Empty>
  );
}

function Detail({ name, listed, ctx, onChanged }: { name: string; listed: boolean; ctx: Ctx; onChanged: () => void }) {
  const mine = ctx.launches.filter((l) => l.experiment === name);
  const busy = mine.some((l) => l.status === "running");
  const q = useLoad<any>("experiment_get", { name }, [mine.length], busy ? 3000 : 0);
  const [error, setError] = useState<string | null>(null);
  const [concurrency, setConcurrency] = useState("");
  const [skipJudge, setSkipJudge] = useState(false);
  const [open, setOpen] = useState<number | null>(null);
  const d = q.data;
  useEffect(() => { setOpen(null); setError(null); }, [name]);
  useEffect(() => { if (open === null && mine[0]) setOpen(mine[0].id); }, [mine.length, name]);
  if (!d) return q.error && !listed ? <Missing name={name} ctx={ctx} /> : <ErrorNote error={q.error} />;
  const def = d.def;
  const launch = async (baselineOnly = false) => {
    setError(null);
    try {
      const r = await call("launch_start", { experiment: name, concurrency: concurrency ? Number(concurrency) : undefined, skip_judge: skipJudge, baseline_only: baselineOnly });
      ctx.reloadLaunches();
      onChanged();
      setOpen(r.launch_id);
    } catch (e: any) { setError(String(e.message ?? e)); }
  };
  return (
    <>
      <div className="row top spread wrap">
        <div className="stack tight">
          <h1>{name}</h1>
          <p className="muted" style={{ maxWidth: "80ch" }}>{def?.description}</p>
        </div>
        <div className="row">
          <button className="btn" onClick={() => ctx.go(`/experiments/${encodeURIComponent(name)}/edit`)}>Edit</button>
        </div>
      </div>
      <ErrorNote error={d.error ?? q.error} />
      {(d.warnings ?? []).map((w: string) => <div key={w} className="note warn">{w}</div>)}

      {def && (
        <Panel title="Plan" sub={`${d.variants?.length ?? 0} variants × ${d.tasks?.length ?? 0} tasks × ${def.runs_per_cell} runs = ${d.total_runs ?? "?"} runs`}>
          <div className="stack">
            <table className="grid">
              <thead><tr><th>Factor</th><th>Kind</th><th>Levels (first is the baseline)</th></tr></thead>
              <tbody>
                {def.factors?.length ? def.factors.map((f: any) => (
                  <tr key={f.name}>
                    <td><b>{f.name}</b></td>
                    <td className="muted">{f.kind.replace(/_/g, " ")}</td>
                    <td><div className="row wrap" style={{ gap: 6 }}>{f.levels.map((l: any, i: number) => <span key={i} className={`chip ${i === 0 ? "" : "accent"}`} title={typeof l === "string" ? l : l.value}>{levelLabel(f.kind, l)}</span>)}</div></td>
                  </tr>
                )) : <tr><td colSpan={3} className="muted">No factors: one variant with the runner defaults.</td></tr>}
              </tbody>
            </table>
            <div className="kv">
              <div><span>Runner</span><b>{def.runner.kind}</b></div>
              <div><span>Model</span><b>{def.runner.model ?? (def.factors?.some((f: any) => f.kind === "model") ? "from factor" : "runner default")}</b></div>
              <div><span>Isolation</span><b>{def.isolation}{def.isolation === "seatbelt" ? `, network ${def.network}` : ""}</b></div>
              <div><span>Timeout per run</span><b>{def.timeout_secs}s</b></div>
              <div><span>Budget per launch</span><b>{def.budget_usd ? `$${def.budget_usd}` : "no cap"}</b></div>
              <div><span>Judge</span><b>{def.graders?.default?.judge ? `${def.graders.default.judge.model}${def.graders.default.judge.pairwise ? " + pairwise" : ""}` : "none"}</b></div>
              <div><span>Human review</span><b>{def.graders?.default?.human?.enabled ? "on" : "off"}</b></div>
            </div>
            <div>
              <h3 style={{ marginBottom: 6 }}>Tasks</h3>
              <table className="grid">
                <thead><tr><th>Task</th><th>What the agent is asked</th><th>What gets judged</th></tr></thead>
                <tbody>
                  {(d.tasks ?? []).map((t: any) => (
                    <tr key={t.id}>
                      <td style={{ whiteSpace: "nowrap" }}><b>{t.id}</b></td>
                      <td>{t.prompt}{t.fixture?.dir && <div className="faint small">starts with the files in {t.fixture.dir}</div>}</td>
                      <td style={{ whiteSpace: "nowrap" }}>{t.output_file ? <code>{t.output_file}</code> : <span className="muted">the final message</span>}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
            <div className="row wrap" style={{ borderTop: "1px solid var(--rule)", paddingTop: 14 }}>
              <button className="btn primary" disabled={!!d.error} onClick={() => launch()}>Run {d.total_runs ?? ""} runs</button>
              {(d.variants?.length ?? 0) > 1 && <button className="btn" disabled={!!d.error} onClick={() => launch(true)} title="Run only the baseline variant first to see whether each task leaves room for a difference. You can run the other variants afterwards.">Baseline only first</button>}
              <label className="row small muted" style={{ gap: 6 }}>
                At a time
                <input type="number" min={1} max={16} placeholder={String(def.runner.concurrency)} value={concurrency} onChange={(e) => setConcurrency(e.target.value)} style={{ width: 64 }} />
              </label>
              {def.graders?.default?.judge && <label className="check small"><input type="checkbox" checked={skipJudge} onChange={(e) => setSkipJudge(e.target.checked)} /> Skip the judge for now</label>}
            </div>
            {d.estimate && (
              <p className="muted small">
                Expect about <b>{duration(d.estimate.minutes * 60)}</b>
                {d.estimate.agent_cost_usd + d.estimate.judge_cost_usd > 0 ? <> and roughly <b>{usd(d.estimate.agent_cost_usd + d.estimate.judge_cost_usd)}</b> of usage</> : null}
                : {d.estimate.runs} agent runs{d.estimate.judge_calls ? `, then ${d.estimate.judge_calls} judge calls` : ""}. Estimated {d.estimate.basis}. You can leave it running and come back; a stopped launch resumes where it left off.
              </p>
            )}
            <ErrorNote error={error} />
          </div>
        </Panel>
      )}

      <Panel title="Launches" flush>
        {mine.length === 0 ? <div className="empty">Not run yet.</div> : (
          <table className="grid">
            <thead><tr><th>Launch</th><th>Started</th><th>Status</th><th className="num">Runs</th><th className="num">Cost</th><th /></tr></thead>
            <tbody>
              {mine.map((l) => (
                <tr key={l.id} className="click" onClick={() => setOpen(open === l.id ? null : l.id)} aria-expanded={open === l.id}>
                  <td><b>#{l.id}</b> <span className="muted small">{l.note}</span></td>
                  <td className="muted">{when(l.created_at)}</td>
                  <td><StatusChip status={l.status} phase={l.phase} /></td>
                  <td className="num">{l.runs_done}/{l.runs_total}{l.runs_failed ? <span className="chip bad" style={{ marginLeft: 6 }}>{l.runs_failed} failed</span> : null}</td>
                  <td className="num">{usd(l.cost_usd)}</td>
                  <td className="num"><a href={`#/results/${l.id}`} onClick={(e) => e.stopPropagation()}>Results</a></td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </Panel>
      {open !== null && mine.some((l) => l.id === open) && <Progress launch={mine.find((l) => l.id === open)!} ctx={ctx} />}
    </>
  );
}

function Progress({ launch, ctx }: { launch: LaunchSummary; ctx: Ctx }) {
  const live = launch.status === "running" || launch.status === "pending";
  const q = useLoad<any>("launch_status", { launch_id: launch.id }, [launch.status], live ? 1000 : 0);
  const [error, setError] = useState<string | null>(null);
  const [cap, setCap] = useState("");
  const s = q.data;
  if (!s) return <ErrorNote error={q.error} />;
  const c = s.counts;
  const act = async (cmd: string, args: Record<string, unknown> = {}) => {
    setError(null);
    try { await call(cmd, { launch_id: launch.id, ...args }); ctx.reloadLaunches(); q.reload(); }
    catch (e: any) { setError(String(e.message ?? e)); }
  };
  const unfinished = c.pending + c.cancelled + c.failed + c.timeout > 0;
  const overBudget = launch.status === "budget";
  const seg = (n: number, color: string) => <i style={{ width: `${(n / Math.max(1, c.total)) * 100}%`, background: color }} />;
  const events: any[] = (s.events ?? []).filter((e: any) => e.event === "rate_limited" || e.event === "warning").slice(-4);
  return (
    <Panel
      title={`Launch #${launch.id}`}
      sub={`${c.done}/${c.total} done${c.running ? `, ${c.running} running` : ""}${c.failed ? `, ${c.failed} failed` : ""}${c.timeout ? `, ${c.timeout} timed out` : ""}. Agent cost ${usd(s.cost_usd)}${s.has_judge ? `, judge ${usd(s.judge_cost_usd)}` : ""}.`}
      actions={
        <div className="row">
          {s.active && <button className="btn danger" onClick={() => act("launch_cancel")}>Cancel</button>}
          {overBudget && <label className="row small muted" style={{ gap: 6 }}>New cap $<input type="number" min={0} step="0.5" value={cap} onChange={(e) => setCap(e.target.value)} style={{ width: 80 }} aria-label="New budget in dollars" /></label>}
          {!s.active && launch.status !== "running" && unfinished && <button className="btn" disabled={overBudget && !(Number(cap) > s.cost_usd + s.judge_cost_usd)} onClick={() => act("launch_resume", { retry_failed: true, budget_usd: overBudget ? Number(cap) : undefined })}>{overBudget ? "Raise cap and resume" : launch.status === "baseline" ? "Run the other variants" : "Resume unfinished runs"}</button>}
          {!s.active && launch.status !== "running" && s.has_judge && c.done > 0 && s.judged < c.done && <button className="btn" onClick={() => act("judge_run")}>Run the judge</button>}
          {s.human_review && c.done > 0 && <a className="btn" href={`#/review/${launch.id}`} style={{ textDecoration: "none" }}>Review</a>}
          {!s.active && launch.status !== "running" && <button className="btn quiet danger" onClick={() => { if (confirm(`Delete launch #${launch.id} and its run artifacts?`)) act("launch_delete"); }}>Delete</button>}
        </div>
      }
    >
      <div className="stack">
        <div className="meter" role="progressbar" aria-valuemin={0} aria-valuemax={c.total} aria-valuenow={c.done}>
          {seg(c.done, "var(--good)")}{seg(c.running, "var(--series)")}{seg(c.timeout, "var(--warn)")}{seg(c.failed, "var(--bad)")}
        </div>
        {s.status === "running" && (
          <p className="muted">
            <b>{PHASE[s.phase] ?? "Working"}</b>
            {s.eta_s ? `, about ${duration(s.eta_s)} left in all` : ""}.
            {" "}{s.phase === "runs" || s.phase === "starting" ? `Runs take about ${Math.round(s.avg_run_s)}s each${s.has_judge ? "; the judge starts when they are all in" : ""}.` : ""}
          </p>
        )}
        {s.has_judge && (s.judge.scored > 0 || s.judge.pairs_done > 0 || s.phase === "judging" || s.phase === "pairwise") && <JudgeFeed j={s.judge} total={c.total} ctx={ctx} live={s.status === "running"} />}
        {overBudget && <div className="note warn">Stopped at the ${s.budget_usd} budget with {c.pending} runs not started. Finished runs are kept. Set a higher cap to continue.</div>}
        {launch.status === "baseline" && <div className="note">Only the baseline has run. <a href={`#/results/${launch.id}`}>Results</a> shows each task's headroom: a task the baseline already passes every time cannot show a benefit. Run the other variants when the tasks look right.</div>}
                {launch.status === "interrupted" && <div className="note warn">The process running this launch stopped. Finished runs are kept; resume to do the rest.</div>}
        {events.map((e, i) => <div key={i} className="note warn">{e.event === "rate_limited" ? `Rate limited on run ${e.run_id}: waiting ${e.wait_s}s before retry ${e.attempt}. ${e.detail}` : e.message}</div>)}
        <ErrorNote error={error ?? q.error} />
        <div className="scrollx">
          {/* minWidth is the sum of the column minimums, so more variants than fit scroll sideways instead of being cut off. */}
          <div className="cells" style={{ gridTemplateColumns: `minmax(140px, 1.2fr) repeat(${s.variants.length}, minmax(110px, 1fr))`, minWidth: 142 + s.variants.length * 111 }}>
            <div className="hd pin">Task</div>
            {s.variants.map((v: any) => <div key={v.key} className="hd clip" title={v.label}>{v.label}</div>)}
            {s.tasks.map((t: string) => [
              <div key={t} className="clip pin" title={t}>{t}</div>,
              ...s.variants.map((v: any) => {
                const cell = s.cells.find((x: any) => x.task_id === t && x.variant_key === v.key);
                return (
                  <div key={`${t}|${v.key}`}>
                    <div className="pips">
                      {(cell?.runs ?? []).map((r: any) => r.status === "pending" || r.status === "running"
                        ? <span key={r.id} className={`pip ${r.status}`} title={`rep ${r.rep + 1}: ${r.status}`} />
                        : <button key={r.id} className={`pip ${r.status}`} title={`rep ${r.rep + 1}: ${r.status}. Open run ${r.id}`} aria-label={`Open run ${r.id}, ${r.status}`} onClick={() => ctx.openRun(r.id)} />)}
                    </div>
                    <div className="faint small">{cell?.cost_usd ? usd(cell.cost_usd) : " "}</div>
                  </div>
                );
              }),
            ])}
          </div>
        </div>
        <p className="faint small">Each square is one run: green done, blue running, amber timed out, red failed. Select a finished square to open the run.</p>
      </div>
    </Panel>
  );
}

const PHASE: Record<string, string> = {
  starting: "Starting",
  runs: "Running the agent on each task",
  judging: "The judge is scoring each result against the task's expectations",
  pairwise: "The judge is comparing results in blind pairs, each pair in both orders",
};

function duration(seconds: number): string {
  if (seconds < 50) return "under a minute";
  if (seconds < 3300) return `${Math.max(1, Math.round(seconds / 60))} min`;
  const h = seconds / 3600;
  return h < 10 ? `${h.toFixed(1)} hours` : `${Math.round(h)} hours`;
}

/** What the judge is doing and what it has said so far, newest first. */
function JudgeFeed({ j, total, ctx, live }: { j: any; total: number; ctx: Ctx; live: boolean }) {
  const [open, setOpen] = useState(live);
  const bar = (done: number, of: number) => (
    <div className="meter" style={{ flex: 1, minWidth: 120 }}><i style={{ width: `${of ? Math.min(100, (done / of) * 100) : 0}%`, background: "var(--series)" }} /></div>
  );
  return (
    <div className="panel" style={{ background: "var(--sunk)" }}>
      <div className="body stack">
        <div className="row spread wrap">
          <h3>What the judge is doing</h3>
          <button className="btn quiet sm" aria-expanded={open} onClick={() => setOpen(!open)}>{open ? "Hide its verdicts" : "Show its verdicts"}</button>
        </div>
        <p className="muted small">
          {j.model} reads each result with the task, its expectations{j.rubric ? ` and the rubric (${j.rubric})` : ""}. It never sees which variant produced a result.
        </p>
        <div className="row wrap"><span className="small" style={{ width: 230 }}>Scored against expectations: <b>{j.scored}</b> of {total}</span>{bar(j.scored, total)}</div>
        {j.pairs_expected > 0 && <div className="row wrap"><span className="small" style={{ width: 230 }}>Blind pairs compared: <b>{j.pairs_done}</b> of {Math.max(j.pairs_expected, j.pairs_done)}</span>{bar(j.pairs_done, Math.max(j.pairs_expected, j.pairs_done))}</div>}
        {open && (
          <>
            {j.pairs.length > 0 && (
              <div>
                <h3 style={{ marginBottom: 4 }}>Latest comparisons</h3>
                <div className="feed">
                  {j.pairs.map((p: any, i: number) => (
                    <div key={i}>
                      <span className="score">{p.winner === "tie" ? "tie" : "won"}</span>
                      <div>
                        <div className="who">{p.task}: {p.a} <span className="faint">vs</span> {p.b}{p.consistent === false ? " (it changed its pick when the order was swapped, so this counts as a tie)" : ""}</div>
                        <div>{p.winner === "tie" ? "" : <b>{p.winner}. </b>}{p.reason}</div>
                      </div>
                    </div>
                  ))}
                </div>
              </div>
            )}
            {j.scores.length > 0 && (
              <div>
                <h3 style={{ marginBottom: 4 }}>Latest scores</h3>
                <div className="feed">
                  {j.scores.map((x: any) => (
                    <div key={x.run_id}>
                      <span className="score">{x.score}<span className="faint small">/10</span></span>
                      <div>
                        <div className="who">{x.task}, {x.variant}{x.of ? `: met ${x.met} of ${x.of} expectations` : ""} <button className="btn quiet sm" style={{ padding: "0 4px" }} onClick={() => ctx.openRun(x.run_id)}>open run {x.run_id}</button></div>
                        <div>{x.summary}</div>
                      </div>
                    </div>
                  ))}
                </div>
              </div>
            )}
            {j.scores.length === 0 && j.pairs.length === 0 && <p className="muted small">Its first verdicts will appear here in a moment.</p>}
          </>
        )}
      </div>
    </div>
  );
}

// ------------------------------------------------------------------ editor

function Editor({ name, listed = true, ctx, onSaved }: { name: string | null; listed?: boolean; ctx: Ctx; onSaved: (name: string) => void }) {
  const [toml, setToml] = useState<string | null>(null);
  const [mode, setMode] = useState<"form" | "toml">("form");
  const [check, setCheck] = useState<any>(null);
  const [error, setError] = useState<string | null>(null);
  const overlays = useLoad<string[]>("overlays_list");
  const seq = useRef(0);
  useEffect(() => {
    setToml(null);
    (name ? call("experiment_get", { name }) : call("experiment_template")).then((r) => setToml(r.toml)).catch((e) => setError(String(e.message ?? e)));
  }, [name]);
  useEffect(() => {
    if (toml === null) return;
    const mine = ++seq.current;
    const t = setTimeout(() => call("experiment_validate", { toml }).then((r) => { if (mine === seq.current) setCheck(r); }).catch(() => {}), 350);
    return () => clearTimeout(t);
  }, [toml]);
  // In form mode the form's own copy is authoritative, so typing never waits
  // on a round trip; in TOML mode the parsed text refreshes it.
  const [formDef, setFormDef] = useState<any>(null);
  const renderSeq = useRef(0);
  useEffect(() => { setFormDef(null); }, [name]);
  useEffect(() => { if (check?.def && (mode === "toml" || !formDef)) setFormDef(check.def); }, [check, mode]);
  if (toml === null) return error && name && !listed ? <Missing name={name} ctx={ctx} /> : <ErrorNote error={error} />;
  const def = formDef;
  const patch = (fn: (d: any) => void) => {
    if (!def) return;
    const next = JSON.parse(JSON.stringify(def));
    fn(next);
    setFormDef(next);
    const mine = ++renderSeq.current;
    call("experiment_render", { def: next })
      .then((r) => { if (mine === renderSeq.current) { setToml(r.toml); setError(null); } })
      .catch((e: any) => { if (mine === renderSeq.current) setError(String(e.message ?? e)); });
  };
  const save = async () => {
    const target = check?.def?.name;
    if (!target) return;
    try {
      await call("experiment_save", { name: target, toml });
      if (name && name !== target) await call("experiment_delete", { name });
      onSaved(target);
    } catch (e: any) { setError(String(e.message ?? e)); }
  };
  const judge = def?.graders?.default?.judge;
  return (
    <>
      <div className="row spread wrap">
        <h1>{name ? `Edit ${name}` : "New experiment"}</h1>
        <div className="row">
          <Seg options={[["form", "Form"], ["toml", "TOML"]]} value={mode} onChange={setMode} />
          <button className="btn" onClick={() => ctx.go(name ? `/experiments/${encodeURIComponent(name)}` : "/experiments")}>Cancel</button>
          <button className="btn primary" disabled={!check?.ok} onClick={save}>Save experiment</button>
        </div>
      </div>
      {check && !check.ok && (/no task files match|selects no tasks/i.test(check.error)
        ? <div className="note warn">{check.error} <button className="btn sm" style={{ marginLeft: 8 }} onClick={() => ctx.go("/experiments/~tasks")}>Open Tasks</button></div>
        : /has no levels/.test(check.error)
          ? <div className="note warn">A factor has no levels yet. Add at least two values to it below: the first is the baseline, the others are compared against it.</div>
          : <div className="note err">{check.error}</div>)}
      {check?.ok && <div className="note">{check.variants.length} variants × {check.tasks} tasks × {check.def.runs_per_cell} runs = {check.total_runs} runs. Saved as <code>experiments/{check.def.name}.toml</code>.</div>}
      {(check?.warnings ?? []).map((w: string) => <div key={w} className="note warn">{w}</div>)}
      <ErrorNote error={error} />

      {mode === "toml" && (
        <Panel title="Definition" sub="The same file the CLI reads.">
          <textarea value={toml} onChange={(e) => setToml(e.target.value)} rows={30} spellCheck={false} aria-label="Experiment TOML" />
        </Panel>
      )}

      {mode === "form" && !def && <div className="note">Fix the definition in the TOML tab to use the form.</div>}
      {mode === "form" && def && (
        <>
          <Panel title="Basics">
            <div className="stack">
              <div className="form-grid">
                <label className="field">Name<input type="text" value={def.name} onChange={(e) => patch((d) => { d.name = e.target.value.replace(/[^\w.-]/g, "-"); })} /></label>
                <label className="field">Runs per cell<input type="number" min={1} value={def.runs_per_cell} onChange={(e) => patch((d) => { d.runs_per_cell = Math.max(1, Number(e.target.value) || 1); })} /></label>
                <label className="field">Timeout per run (s)<input type="number" min={10} value={def.timeout_secs} onChange={(e) => patch((d) => { d.timeout_secs = Number(e.target.value) || 900; })} /></label>
                <label className="field">Isolation
                  <select value={def.isolation} onChange={(e) => patch((d) => { d.isolation = e.target.value; })}>
                    <option value="workspace">Workspace copy</option>
                    <option value="seatbelt">Seatbelt sandbox</option>
                    <option value="docker">Docker container</option>
                  </select>
                </label>
                {def.isolation === "docker" && (
                  <label className="field">Docker image<input type="text" placeholder="image with the runner CLI" value={def.runner.docker_image ?? ""} onChange={(e) => patch((d) => { d.runner.docker_image = e.target.value || undefined; })} /></label>
                )}
                <label className="field">Budget for one launch ($)<input type="number" min={0} step="0.5" placeholder="no cap" value={def.budget_usd ?? ""} onChange={(e) => patch((d) => { d.budget_usd = Number(e.target.value) > 0 ? Number(e.target.value) : undefined; })} /></label>
                {def.isolation === "seatbelt" && (
                  <label className="field">Network
                    <select value={def.network} onChange={(e) => patch((d) => { d.network = e.target.value; })}>
                      <option value="full">Unrestricted</option>
                      <option value="anthropic">Anthropic hosts only</option>
                    </select>
                  </label>
                )}
                <label className="field">Primary metric
                  <select value={def.primary_metric ?? ""} onChange={(e) => patch((d) => { d.primary_metric = e.target.value || undefined; })}>
                    <option value="">Automatic</option>
                    <option value="judge_score">Judge score</option>
                    <option value="pass">Pass rate</option>
                    <option value="expectations_passed">Expectations met</option>
                    <option value="cost_usd">Cost</option>
                  </select>
                </label>
              </div>
              <label className="field">Question this experiment answers<input type="text" value={def.description ?? ""} onChange={(e) => patch((d) => { d.description = e.target.value; })} /></label>
            </div>
          </Panel>

          <Panel title="Runner" sub="Defaults for anything a factor does not vary.">
            <div className="form-grid">
              <label className="field">Runner
                <select value={def.runner.kind} onChange={(e) => patch((d) => { d.runner.kind = e.target.value; })}>
                  <option value="claude-code">Claude Code</option>
                  <option value="codex">Codex</option>
                  <option value="mock">Mock (offline)</option>
                </select>
              </label>
              {(def.factors ?? []).some((f: any) => f.kind === "model")
                ? <label className="field">Model<input type="text" disabled value="set by the model factor below" /></label>
                : <label className="field">Model<input type="text" placeholder="runner default" value={def.runner.model ?? ""} onChange={(e) => patch((d) => { d.runner.model = e.target.value || undefined; })} /></label>}
              {(def.factors ?? []).some((f: any) => f.kind === "effort")
                ? <label className="field">Effort<input type="text" disabled value="set by the effort factor below" /></label>
                : (
                  <label className="field">Effort
                    <select value={def.runner.effort ?? ""} onChange={(e) => patch((d) => { d.runner.effort = e.target.value || undefined; })}>
                      <option value="">Runner default</option>
                      {["low", "medium", "high", "xhigh", "max"].map((x) => <option key={x}>{x}</option>)}
                    </select>
                  </label>
                )}
              <label className="field">Runs at a time<input type="number" min={1} max={16} value={def.runner.concurrency} onChange={(e) => patch((d) => { d.runner.concurrency = Math.max(1, Number(e.target.value) || 1); })} /></label>
              <label className="field">Max turns<input type="number" min={1} placeholder="no limit" value={def.runner.max_turns ?? ""} onChange={(e) => patch((d) => { d.runner.max_turns = e.target.value ? Number(e.target.value) : undefined; })} /></label>
            </div>
          </Panel>

          <Panel title="Factors" sub="A factor is one thing you vary. Each factor has levels (its values); every combination of levels is run on every task.">
            <div className="stack">
              {(def.factors ?? []).length === 0 && <p className="muted">No factors yet. Add what you want to compare: for example a skill against no skill, or three models.</p>}
              {(def.factors ?? []).map((f: any, i: number) => {
                const plain = f.levels.every((l: any) => typeof l === "string");
                const help = FACTOR_HELP[f.kind];
                return (
                  <div key={i} className="stack tight" style={{ borderBottom: "1px solid var(--rule)", paddingBottom: 14 }}>
                    <div className="row wrap top">
                      <label className="field">What varies
                        <select value={f.kind} onChange={(e) => patch((d) => {
                          const kind = e.target.value;
                          const wasDefault = !d.factors[i].name || FACTOR_KINDS.some(([id]) => id === d.factors[i].name) || /^factor\d+$/.test(d.factors[i].name);
                          d.factors[i].kind = kind;
                          d.factors[i].levels = FACTOR_HELP[kind].start;
                          if (wasDefault) d.factors[i].name = FACTOR_HELP[kind].name;
                        })}>
                          {FACTOR_KINDS.map(([id, label]) => <option key={id} value={id}>{label}</option>)}
                        </select>
                      </label>
                      <label className="field">Name in reports<input type="text" value={f.name} onChange={(e) => patch((d) => { d.factors[i].name = e.target.value; })} /></label>
                      {f.kind === "knob" && <label className="field">CLI flag<input type="text" placeholder="--max-turns" value={f.flag ?? ""} onChange={(e) => patch((d) => { d.factors[i].flag = e.target.value || undefined; })} /></label>}
                      {f.kind === "knob" && <label className="field">or env var<input type="text" value={f.env ?? ""} onChange={(e) => patch((d) => { d.factors[i].env = e.target.value || undefined; })} /></label>}
                      <span className="grow" />
                      <button className="btn quiet danger sm" onClick={() => patch((d) => { d.factors.splice(i, 1); })}>Remove factor</button>
                    </div>
                    {plain ? (
                      <LevelChips
                        kind={f.kind}
                        levels={f.levels}
                        help={help}
                        suggestions={f.kind === "overlay" ? ["overlays/none", ...(overlays.data ?? []).filter((o) => !o.includes("/ablation/") && !o.endsWith("/none"))] : help.suggest}
                        onChange={(levels) => patch((d) => { d.factors[i].levels = levels; })}
                      />
                    ) : <p className="muted small">These levels carry labels or per-level settings. Edit them in the TOML tab.</p>}
                  </div>
                );
              })}
              <div className="row wrap">
                <span className="muted small">Add a factor:</span>
                {FACTOR_KINDS.map(([id, label]) => (
                  <button key={id} className="btn sm" disabled={["model", "effort", "runner"].includes(id) && (def.factors ?? []).some((f: any) => f.kind === id)}
                    onClick={() => patch((d) => { d.factors = [...(d.factors ?? []), { name: uniqueName(d.factors ?? [], FACTOR_HELP[id].name), kind: id, levels: FACTOR_HELP[id].start }]; })}>{label.split(" (")[0]}</button>
                ))}
              </div>
            </div>
          </Panel>

          <Panel title="Tasks" sub="Glob patterns relative to the project folder, one per line.">
            <LevelsBox levels={def.tasks?.include ?? []} rows={3} onCommit={(include) => patch((d) => { d.tasks = { ...(d.tasks ?? {}), include }; })} />
          </Panel>

          <Panel title="Grading" sub="Deterministic graders come from each task file. These settings add the LLM judge and human review.">
            <div className="stack">
              <label className="check"><input type="checkbox" checked={!!judge} onChange={(e) => patch((d) => { d.graders = d.graders ?? {}; d.graders.default = d.graders.default ?? {}; d.graders.default.judge = e.target.checked ? { model: "claude-opus-5-5", pairwise: true, rubric_scoring: true } : undefined; })} /> Score runs with an LLM judge</label>
              {judge && (
                <div className="form-grid">
                  <label className="field">Judge model<input type="text" value={judge.model} onChange={(e) => patch((d) => { d.graders.default.judge.model = e.target.value; })} /></label>
                  <label className="field">Rubric file<input type="text" placeholder="rubrics/…md (optional)" value={judge.rubric ?? ""} onChange={(e) => patch((d) => { d.graders.default.judge.rubric = e.target.value || undefined; })} /></label>
                  <label className="check" style={{ alignSelf: "end" }}><input type="checkbox" checked={!!judge.pairwise} onChange={(e) => patch((d) => { d.graders.default.judge.pairwise = e.target.checked; })} /> Blind pairwise, both orders</label>
                </div>
              )}
              <label className="check"><input type="checkbox" checked={!!def.graders?.default?.human?.enabled} onChange={(e) => patch((d) => { d.graders = d.graders ?? {}; d.graders.default = d.graders.default ?? {}; d.graders.default.human = { enabled: e.target.checked }; })} /> Queue pairs for human review</label>
            </div>
          </Panel>
        </>
      )}
    </>
  );
}

interface FactorHelp { name: string; start: string[]; suggest: string[]; placeholder: string; hint: string }
const FACTOR_HELP: Record<string, FactorHelp> = {
  overlay: { name: "overlay", start: ["overlays/none"], suggest: [], placeholder: "Folder holding a skill or CLAUDE.md", hint: "Each level is a folder: a skill folder (with SKILL.md), or an overlay folder laid out like a project (.claude/skills, CLAUDE.md). overlays/none is the empty baseline." },
  model: { name: "model", start: [], suggest: ["claude-opus-5-5", "claude-opus-5", "claude-opus-4-8", "claude-sonnet-5-5", "claude-haiku-5-5", "claude-fable-5-1"], placeholder: "Model id, for example claude-opus-5-5", hint: "Each level is a model id. Add every model you want to compare." },
  effort: { name: "effort", start: ["medium", "high"], suggest: ["low", "medium", "high", "xhigh", "max"], placeholder: "Effort level", hint: "Each level is an effort setting." },
  runner: { name: "runner", start: ["claude-code"], suggest: ["claude-code", "codex", "mock"], placeholder: "Runner", hint: "Each level is an agent harness. To give each runner its own model, edit the levels in the TOML tab." },
  system_prompt_append: { name: "append", start: [""], suggest: [], placeholder: "Text to append, or file:path/to/file.md", hint: "Each level is text appended to the system prompt. The empty level means nothing is appended." },
  knob: { name: "flag", start: [], suggest: [], placeholder: "Value for the flag or variable", hint: "Each level is a value passed to the CLI flag or environment variable above." },
};

function uniqueName(factors: any[], base: string): string {
  if (!factors.some((f) => f.name === base)) return base;
  for (let n = 2; ; n++) if (!factors.some((f) => f.name === `${base}${n}`)) return `${base}${n}`;
}

/** The levels of one factor as removable chips, with an input and suggestions to add more. */
function LevelChips({ kind, levels, help, suggestions, onChange }: { kind: string; levels: string[]; help: FactorHelp; suggestions: string[]; onChange: (levels: string[]) => void }) {
  const [draft, setDraft] = useState("");
  const add = (v: string) => {
    const value = v.trim();
    if ((value === "" && kind !== "system_prompt_append") || levels.includes(value)) return;
    onChange([...levels, value]);
    setDraft("");
  };
  const open = suggestions.filter((x) => !levels.includes(x));
  return (
    <div className="stack tight">
      <div className="small muted">Levels <span className="faint">({help.hint})</span></div>
      <div className="row wrap" style={{ gap: 6 }}>
        {levels.length === 0 && <span className="chip warn">No levels yet: add at least two</span>}
        {levels.map((l, i) => (
          <span key={l || "(none)"} className={`chip ${i === 0 ? "" : "accent"}`} title={l || "nothing"} style={{ paddingRight: 2 }}>
            {levelLabel(kind, l)}
            {i === 0 ? <span className="faint" style={{ fontWeight: 400 }}>baseline</span>
              : <button className="btn quiet sm" style={{ padding: "0 4px", fontWeight: 400 }} title="Compare the other levels against this one" onClick={() => onChange([l, ...levels.filter((x) => x !== l)])}>make baseline</button>}
            <button className="btn quiet sm" style={{ padding: "0 5px" }} aria-label={`Remove ${l || "empty level"}`} onClick={() => onChange(levels.filter((x) => x !== l))}>×</button>
          </span>
        ))}
      </div>
      <div className="row wrap">
        <input type="text" value={draft} placeholder={help.placeholder} aria-label={`Add a ${kind} level`} style={{ width: 340 }}
          onChange={(e) => setDraft(e.target.value)} onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); add(draft); } }} />
        <button className="btn sm" disabled={!draft.trim()} onClick={() => add(draft)}>Add level</button>
      </div>
      {open.length > 0 && (
        <div className="row wrap small" style={{ gap: 6 }}>
          <span className="muted">Add:</span>
          {open.slice(0, 16).map((x) => <button key={x} className="btn sm" title={x} onClick={() => add(x)}>{kind === "overlay" ? x.replace(/^overlays\//, "") : x}</button>)}
        </div>
      )}
    </div>
  );
}

/** Task files: what the agent is asked to do, with fixtures and graders. */
function Tasks({ ctx }: { ctx: Ctx }) {
  const list = useLoad<any[]>("tasks_list");
  const [source, setSource] = useState<string | null>(null);
  const [isNew, setIsNew] = useState(false);
  const [toml, setToml] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  const tasks = list.data ?? [];
  const openTask = async (src: string) => {
    setError(null); setSaved(false); setIsNew(false); setSource(src);
    try { setToml((await call("task_get", { source: src })).toml); } catch (e: any) { setError(String(e.message ?? e)); }
  };
  const start = async () => {
    setError(null); setSaved(false); setIsNew(true); setSource("tasks/my-task.toml");
    setToml((await call("task_template")).toml);
  };
  const save = async () => {
    if (!source) return;
    try { await call("task_save", { source, toml }); setSaved(true); setIsNew(false); list.reload(); } catch (e: any) { setError(String(e.message ?? e)); setSaved(false); }
  };
  const remove = async () => {
    if (!source || !confirm(`Delete ${source}?`)) return;
    try { await call("task_delete", { source }); setSource(null); list.reload(); } catch (e: any) { setError(String(e.message ?? e)); }
  };
  return (
    <>
      <div className="row spread wrap top">
        <div className="stack tight">
          <h1>Tasks</h1>
          <p className="muted" style={{ maxWidth: "76ch" }}>A task is a prompt, the files the agent starts with, and how the result is graded. Each task is one file under <code>tasks/</code>; experiments pick tasks by pattern, for example <code>tasks/**/*.toml</code>.</p>
        </div>
        <div className="row">
          <button className="btn primary" onClick={start}>New task</button>
          <button className="btn" onClick={() => ctx.go("/experiments/new")}>New experiment</button>
        </div>
      </div>
      <ErrorNote error={list.error} />
      <Panel title="Task files" flush>
        {tasks.length === 0 ? <div className="empty">No tasks yet. Create the first one; three to six tasks that differ from each other give a far more trustworthy answer than one.</div> : (
          <table className="grid">
            <thead><tr><th>Task</th><th>File</th><th className="num">Expectations</th><th className="num">Graders</th></tr></thead>
            <tbody>
              {tasks.map((t, i) => t.error ? <tr key={i}><td colSpan={4}><span className="chip bad">invalid</span> {t.error}</td></tr> : (
                <tr key={t.source} className="click" onClick={() => openTask(t.source)} style={source === t.source ? { background: "var(--accent-soft)" } : undefined}>
                  <td><b>{t.id}</b> {(t.tags ?? []).map((x: string) => <span key={x} className="tag" style={{ marginLeft: 4 }}>{x}</span>)}<div className="muted small clip" style={{ maxWidth: 520 }}>{t.prompt}</div></td>
                  <td className="mono muted">{t.source}</td>
                  <td className="num">{t.expectations}</td>
                  <td className="num">{t.deterministic}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </Panel>
      {source !== null && (
        <Panel title={isNew ? "New task" : source} sub="Fields are documented in docs/formats.md. Fixture folders live under fixtures/."
          actions={<div className="row">{!isNew && <button className="btn quiet danger" onClick={remove}>Delete</button>}<button className="btn primary" onClick={save}>Save task</button></div>}>
          <div className="stack">
            {isNew && <label className="field">File<input type="text" value={source} onChange={(e) => setSource(e.target.value)} style={{ maxWidth: 420 }} /></label>}
            <textarea rows={22} value={toml} spellCheck={false} aria-label="Task TOML" onChange={(e) => { setToml(e.target.value); setSaved(false); }} />
            <ErrorNote error={error} />
            {saved && <div className="note">Saved {source}.</div>}
          </div>
        </Panel>
      )}
    </>
  );
}

/** One value per line; commits on blur so typing is not interrupted by re-rendering. */
function LevelsBox({ levels, onCommit, rows }: { levels: string[]; onCommit: (levels: string[]) => void; rows?: number }) {
  const [text, setText] = useState(levels.join("\n"));
  const joined = levels.join("\n");
  useEffect(() => { setText(joined); }, [joined]);
  return (
    <textarea
      rows={rows ?? Math.max(2, levels.length + 1)}
      value={text}
      spellCheck={false}
      aria-label="Values, one per line"
      onChange={(e) => setText(e.target.value)}
      onBlur={() => { const next = text.split("\n").map((s) => s.trim()).filter(Boolean); if (next.join("\n") !== joined) onCommit(next); }}
    />
  );
}
