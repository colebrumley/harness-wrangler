import { useState } from "react";
import type { Ctx } from "../App";
import { Empty, ErrorNote, Panel } from "../components";
import { useLoad } from "../hooks";

const KIND: Record<string, string> = {
  remove_section: "Remove section", review_section: "Review section", keep_section: "Keep section", retire_pattern: "Retire", drop_pattern: "Drop",
  keep_pattern: "Keep", fix_trigger: "Fix triggering", trim_body: "Trim", change_level: "Change setting", tradeoff: "Trade-off", flaky_task: "Flaky task",
  more_runs: "Needs more runs", judge_bias: "Judge reliability", reviewer_flag: "Reviewers flagged", failures: "Failures",
};

export default function Advice({ ctx }: { ctx: Ctx }) {
  const id = ctx.launchId;
  const launch = ctx.launches.find((l) => l.id === id);
  const q = useLoad<any>(id ? "advice" : null, { launch_id: id }, [launch?.runs_done, launch?.status]);
  const [open, setOpen] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  if (!id) return <div className="content"><Empty title="No advice yet">Advice is drawn from a launch's results.</Empty></div>;
  const d = q.data;
  const list: any[] = d?.suggestions ?? [];
  const exportJson = async () => {
    const text = JSON.stringify(d, null, 2);
    try { await navigator.clipboard.writeText(text); setCopied(true); setTimeout(() => setCopied(false), 1800); }
    catch {
      const a = document.createElement("a");
      a.href = URL.createObjectURL(new Blob([text], { type: "application/json" }));
      a.download = `advice-launch-${id}.json`;
      a.click();
    }
  };
  return (
    <div className="content">
      <div className="page">
        <div className="row spread top wrap">
          <div className="stack tight">
            <h1>Advice</h1>
            <p className="muted" style={{ maxWidth: "78ch" }}>
              What to change, most urgent first. Each item names its metric and the measured effect with its 95% interval.
              {d ? ` From launch #${d.launch_id} of ${d.experiment}.` : ""}
            </p>
          </div>
          <div className="row">
            <button className="btn" disabled={!d} onClick={exportJson}>{copied ? "Copied" : "Copy as JSON"}</button>
          </div>
        </div>
        <ErrorNote error={q.error} />
        {d && list.length === 0 && <Empty title="Nothing to act on yet">No comparison in this launch is conclusive enough to recommend a change. More runs per cell narrow the intervals.</Empty>}
        {list.length > 0 && (
          <Panel flush>
            {list.map((s, i) => (
              <div key={s.id} className={`sugg p${s.priority} ${s.kind.startsWith("keep") ? "keep" : ""}`}>
                <div className="pri" aria-label={`Priority ${s.priority}`}>{i + 1}</div>
                <div className="stack tight">
                  <div className="row wrap">
                    <span className="chip">{KIND[s.kind] ?? s.kind}</span>
                    <span className="faint small">{s.confidence} confidence, measured on {s.metric.replace(/_/g, " ")}</span>
                  </div>
                  <p className="act">{s.action}</p>
                </div>
                <div className="row">
                  <button className="btn quiet sm" aria-expanded={open === s.id} onClick={() => setOpen(open === s.id ? null : s.id)}>{open === s.id ? "Hide evidence" : "Evidence"}</button>
                  <a className="btn quiet sm" href={`#/${s.target?.type === "section" || s.target?.type === "file" ? "ablation" : "results"}/${id}`}>Open</a>
                </div>
                {open === s.id && <pre>{JSON.stringify({ id: s.id, target: s.target, evidence: s.evidence }, null, 2)}</pre>}
              </div>
            ))}
          </Panel>
        )}
        <p className="faint small">The same list is available to agents and scripts: <code>wrangler advice {id} --json</code></p>
      </div>
    </div>
  );
}
