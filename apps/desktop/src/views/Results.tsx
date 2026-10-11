import { useEffect, useState } from "react";
import type { Ctx } from "../App";
import { Comparison, Report, fmtValue, info, remember, usd } from "../api";
import { Empty, ErrorNote, Forest, MeanBar, Panel, Seg, VerdictMark, equivalenceBand } from "../components";
import { useLoad } from "../hooks";

export default function Results({ ctx }: { ctx: Ctx }) {
  const id = ctx.launchId;
  const launch = ctx.launches.find((l) => l.id === id);
  const [metric, setMetric] = useState<string | null>(null);
  useEffect(() => setMetric(null), [id]);
  const q = useLoad<Report>(id ? "report" : null, { launch_id: id, metric: metric ?? undefined }, [launch?.runs_done, launch?.status], launch?.status === "running" ? 4000 : 0);
  const [cell, setCell] = useState<{ task: string; variant: string } | null>(null);
  useEffect(() => setCell(null), [id]);
  if (!id) return <div className="content"><Empty title="No results yet">Run an experiment and its results appear here.</Empty></div>;
  const r = q.data;
  if (!r) return <div className="content"><ErrorNote error={q.error} />{q.loading && <p className="muted">Computing the report…</p>}</div>;
  remember(r.metrics);
  const pm = info(r.primary_metric);
  const quality = r.metrics.filter((m) => m.direction === "higher");
  const has = (m: string) => r.metrics.some((x) => x.name === m);
  const side = ["cost_usd", "wall_time_s"].filter(has);
  const comps = (kind: string, factor: string | null, m: string) => r.comparisons.filter((c) => c.kind === kind && c.factor === factor && c.metric === m);

  return (
    <div className="content">
      <div className="page">
        <div className="row top spread wrap">
          <div className="stack tight">
            <h1>{r.experiment}</h1>
            <p className="muted" style={{ maxWidth: "80ch" }}>{r.description}</p>
          </div>
          <label className="row small muted" style={{ gap: 6 }}>
            Quality metric
            <select value={r.primary_metric} onChange={(e) => setMetric(e.target.value)}>
              {r.metrics.map((m) => <option key={m.name} value={m.name}>{m.label}</option>)}
            </select>
          </label>
        </div>
        <div className="kv">
          <div><span>Runs finished</span><b>{r.counts.done} of {r.counts.total}</b></div>
          <div><span>Failed or timed out</span><b>{r.counts.failed + r.counts.timeout}</b></div>
          <div><span>Agent cost</span><b>{usd(r.cost_usd)}</b></div>
          <div><span>Judge cost</span><b>{usd(r.judge_cost_usd)}</b></div>
          <div><span>Runs per cell</span><b>{r.runs_per_cell}</b></div>
          <div><span>Definition</span><b className="mono">{r.def_hash}</b></div>
        </div>
        {r.warnings.map((w) => <div key={w} className="note warn">{w}</div>)}
        {r.counts.done === 0 && <Empty title="Nothing finished yet">Results fill in as runs complete.</Empty>}

        {r.factors.map((f) => {
          const main = comps("factor", f.name, r.primary_metric);
          if (main.length === 0) return null;
          return (
            <Panel key={f.name} title={`Effect of ${f.name}`} sub={`Each level against "${f.levels[0]}", paired by task${r.factors.length > 1 ? " and the other factors" : ""}. Whiskers are 95% bootstrap intervals; the shaded band is too small to matter.`}>
              <div className="stack">
                <EffectBlock comparisons={main} metric={r.primary_metric} />
                {side.map((m) => comps("factor", f.name, m).length > 0 && (
                  <div key={m}>
                    <h3 style={{ margin: "6px 0 4px" }}>{info(m).label}</h3>
                    <EffectBlock comparisons={comps("factor", f.name, m)} metric={m} />
                  </div>
                ))}
              </div>
            </Panel>
          );
        })}

        {r.variants.length > 0 && r.counts.done > 0 && (
          <Panel title="Variants" sub="Mean with its 95% interval. Cost and time sit beside quality so the trade is visible." flush>
            <div className="scrollx">
              <table className="grid">
                <thead>
                  <tr>
                    <th>Variant</th><th className="num">Runs</th>
                    <th colSpan={2}>{pm.label}</th>
                    {["pass", "skill_invoked", "cost_usd", "wall_time_s", "total_tokens", "turns"].filter((m) => has(m) && m !== r.primary_metric).map((m) => <th key={m} className="num">{info(m).label}</th>)}
                    {r.factors.length > 1 && <th>vs first variant</th>}
                  </tr>
                </thead>
                <tbody>
                  {r.variants.map((v, i) => {
                    const s = v.metrics[r.primary_metric];
                    const max = pm.scale === "rate" ? 1 : pm.scale === "score10" ? 10 : Math.max(...r.variants.map((x) => x.metrics[r.primary_metric]?.ci_high ?? 0));
                    const vs = r.comparisons.find((c) => c.kind === "variant" && c.variant_key === v.key && c.metric === r.primary_metric);
                    return (
                      <tr key={v.key}>
                        <td><b>{v.label}</b>{v.counts.failed + v.counts.timeout > 0 && <span className="chip bad" style={{ marginLeft: 8 }}>{v.counts.failed + v.counts.timeout} failed</span>}</td>
                        <td className="num">{v.counts.done}</td>
                        <td className="num" style={{ width: 70 }}><b>{fmtValue(pm, s?.mean)}</b></td>
                        <td style={{ width: 200 }}>{s && <MeanBar mean={s.mean} lo={s.ci_low} hi={s.ci_high} max={max} />}</td>
                        {["pass", "skill_invoked", "cost_usd", "wall_time_s", "total_tokens", "turns"].filter((m) => has(m) && m !== r.primary_metric).map((m) => (
                          <td key={m} className="num" title={v.metrics[m] ? `sd ${fmtValue(info(m), v.metrics[m].sd)}, n=${v.metrics[m].n}` : ""}>{fmtValue(info(m), v.metrics[m]?.mean)}</td>
                        ))}
                        {r.factors.length > 1 && <td>{i === 0 ? <span className="faint">baseline</span> : vs ? <VerdictMark verdict={vs.verdict} /> : null}</td>}
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
          </Panel>
        )}

        {r.tasks.length > 0 && r.counts.done > 0 && (
          <Panel title="By task" sub={`${pm.label} per task and variant. Select a cell to see its runs.`}
            actions={quality.length > 1 ? <Seg options={quality.slice(0, 4).map((m) => [m.name, m.label] as [string, string])} value={r.primary_metric} onChange={setMetric} /> : undefined} flush>
            <div className="scrollx">
              <table className="grid">
                <thead><tr><th>Task</th>{r.variants.map((v) => <th key={v.key} className="num">{v.label}</th>)}</tr></thead>
                <tbody>
                  {r.tasks.map((t) => (
                    <tr key={t.task_id}>
                      <td><b>{t.task_id}</b> {t.tags.map((x) => <span key={x} className="tag" style={{ marginLeft: 4 }}>{x}</span>)}</td>
                      {t.cells.map((c) => {
                        const s = c.metrics[r.primary_metric];
                        const on = cell?.task === t.task_id && cell?.variant === c.variant_key;
                        const flaky = c.flaky.includes(r.primary_metric);
                        return (
                          <td key={c.variant_key} className="num">
                            <button className="heat btn quiet" style={{ color: "inherit", background: on ? "var(--accent-soft)" : undefined }} onClick={() => setCell(on ? null : { task: t.task_id, variant: c.variant_key })}
                              aria-expanded={on}
                              aria-label={`${t.task_id}, ${r.variants.find((v) => v.key === c.variant_key)?.label}: ${s ? `${pm.label} ${fmtValue(pm, s.mean)}, sd ${fmtValue(pm, s.sd)}, ${s.n} runs` : "no data"}. Show runs.`}>
                              <b>{fmtValue(pm, s?.mean)}</b>
                              {s && <span className="faint small"> ±{fmtValue(pm, s.sd).replace("$", "")}</span>}
                              {flaky && <span className="chip warn" style={{ marginLeft: 6 }} title="High variance across repetitions">flaky</span>}
                              {c.counts.failed + c.counts.timeout > 0 && <span className="chip bad" style={{ marginLeft: 6 }}>{c.counts.failed + c.counts.timeout} failed</span>}
                            </button>
                          </td>
                        );
                      })}
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
            {cell && <CellRuns launchId={id} task={cell.task} variant={cell.variant} ctx={ctx} />}
          </Panel>
        )}

        {r.pairwise && <PairwisePanel p={r.pairwise} />}
      </div>
    </div>
  );
}

function EffectBlock({ comparisons, metric }: { comparisons: Comparison[]; metric: string }) {
  const m = info(metric);
  const band = equivalenceBand(m, comparisons[0]?.base_mean ?? 0);
  return (
    <Forest
      metric={m}
      labelHead={m.label}
      band={band}
      rows={comparisons.map((c) => ({ key: c.level, label: c.level, sub: `vs ${c.baseline}: ${fmtValue(m, c.base_mean)} → ${fmtValue(m, c.treat_mean)}`, effect: c }))}
    />
  );
}

function CellRuns({ launchId, task, variant, ctx }: { launchId: number; task: string; variant: string; ctx: Ctx }) {
  const q = useLoad<any[]>("runs_list", { launch_id: launchId }, [task, variant]);
  const rows = (q.data ?? []).filter((r) => r.task_id === task && r.variant_key === variant);
  return (
    <div style={{ borderTop: "1px solid var(--rule)", background: "var(--sunk)" }}>
      <table className="grid">
        <thead><tr><th style={{ background: "var(--sunk)" }}>Run</th>{["Status", "Judge", "Pass", "Cost", "Time", "Turns"].map((h) => <th key={h} className="num" style={{ background: "var(--sunk)" }}>{h}</th>)}</tr></thead>
        <tbody>
          {rows.map((r) => (
            <tr key={r.id} className="click" onClick={() => ctx.openRun(r.id)}>
              <td><b>Run {r.id}</b> <span className="muted">rep {r.rep + 1}</span> {r.error && <span className="muted small clip">{r.error.split("\n")[0]}</span>}</td>
              <td className="num">{r.status}</td>
              <td className="num">{fmtValue(info("judge_score"), r.judge_score)}</td>
              <td className="num">{r.pass === null || r.pass === undefined ? "–" : r.pass ? "yes" : "no"}</td>
              <td className="num">{r.cost_usd === null || r.cost_usd === undefined ? "–" : usd(r.cost_usd)}</td>
              <td className="num">{fmtValue(info("wall_time_s"), r.wall_time_s)}</td>
              <td className="num">{r.turns ?? "–"}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function PairwisePanel({ p }: { p: any }) {
  const [source, setSource] = useState<string>(p.rankings[0]?.source ?? "judge");
  const rk = p.rankings.find((x: any) => x.source === source) ?? p.rankings[0];
  if (!rk) return null;
  const order = p.variants.map((_: string, i: number) => i).sort((a: number, b: number) => rk.strengths[b].strength - rk.strengths[a].strength);
  const max = Math.max(...rk.strengths.map((s: any) => s.ci_high), 0.0001);
  const pb = p.position_bias;
  const labels: Record<string, string> = { judge: "LLM judge", human: "Human", all: "Both" };
  return (
    <Panel title="Blind pairwise preference" sub={`${rk.votes} votes. A win counts only when both presentation orders agree.`}
      actions={p.rankings.length > 1 ? <Seg options={p.rankings.map((x: any) => [x.source, labels[x.source] ?? x.source] as [string, string])} value={rk.source} onChange={setSource} /> : undefined}>
      <div className="stack">
        {p.variants.length > 2 && (
          <table className="grid">
            <thead><tr><th>Variant</th><th colSpan={2}>Bradley–Terry strength, 95% interval</th><th className="num">Won</th><th className="num">Lost</th><th className="num">Tied</th></tr></thead>
            <tbody>
              {order.map((i: number) => {
                const s = rk.strengths[i];
                return (
                  <tr key={i}>
                    <td><b>{p.variants[i]}</b></td>
                    <td className="num" style={{ width: 70 }}>{s.strength.toFixed(3)}</td>
                    <td style={{ width: 260 }}><MeanBar mean={s.strength} lo={s.ci_low} hi={s.ci_high} max={max} /></td>
                    <td className="num">{s.wins}</td><td className="num">{s.losses}</td><td className="num">{s.ties}</td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        )}
        <table className="grid">
          <thead><tr><th>Pair</th><th className="num">Wins</th><th className="num">Ties</th><th className="num">Both bad</th><th colSpan={2}>Preference for the second</th></tr></thead>
          <tbody>
            {rk.pairs.map((pc: any) => (
              <tr key={`${pc.a}|${pc.b}`}>
                <td>{pc.a} <span className="faint">vs</span> <b>{pc.b}</b></td>
                <td className="num">{pc.a_wins} : {pc.b_wins}</td>
                <td className="num">{pc.ties}</td>
                <td className="num">{pc.both_bad}</td>
                <td className="num" style={{ width: 150 }}>{pc.b_rate ? `${Math.round(pc.b_rate.mean * 100)}% [${Math.round(pc.b_rate.ci_low * 100)}, ${Math.round(pc.b_rate.ci_high * 100)}]` : "–"}</td>
                <td style={{ width: 200 }}>{pc.b_rate && <MeanBar mean={pc.b_rate.mean} lo={pc.b_rate.ci_low} hi={pc.b_rate.ci_high} max={1} />}</td>
              </tr>
            ))}
          </tbody>
        </table>
        {pb && (
          <div className={`note ${pb.inconsistent_rate > 0.3 ? "warn" : ""}`}>
            Position bias check: the judge changed its pick when the order was swapped on {pb.inconsistent} of {pb.pairs} pairs ({Math.round(pb.inconsistent_rate * 100)}%).
            The first-listed result won {Math.round(pb.first_position_rate * 100)}% of {pb.decided_picks} decided picks; 50% is unbiased.
          </div>
        )}
        {p.human_judge_agreement !== null && p.human_judge_agreement !== undefined && (
          <div className="note">Human and judge agreed on {Math.round(p.human_judge_agreement * 100)}% of the {p.human_judge_overlap} pairs both have judged.</div>
        )}
      </div>
    </Panel>
  );
}
