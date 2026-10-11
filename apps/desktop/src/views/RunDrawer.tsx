import { useEffect, useState } from "react";
import { fmtValue, info } from "../api";
import { ErrorNote, StatusChip, Tabs } from "../components";
import { useLoad } from "../hooks";
import { TextLines } from "./TextLines";

type Tab = "output" | "diff" | "transcript" | "grades" | "metrics";

/** One run in full. In blind mode (opened from Review) nothing that identifies the variant is shown. */
export default function RunDrawer({ runId, blind, onClose }: { runId: number; blind: boolean; onClose: () => void }) {
  const q = useLoad<any>("run_get", { run_id: runId });
  const [tab, setTab] = useState<Tab>(blind ? "transcript" : "output");
  useEffect(() => {
    const on = (e: KeyboardEvent) => { if (e.key === "Escape") onClose(); };
    window.addEventListener("keydown", on);
    return () => window.removeEventListener("keydown", on);
  }, [onClose]);
  const d = q.data;
  const tabs: [Tab, string][] = blind
    ? [["transcript", "Transcript"], ["output", "Output"], ["diff", "Diff"]]
    : [["output", "Output"], ["diff", "Diff"], ["transcript", "Transcript"], ["grades", "Grades"], ["metrics", "Metrics"]];
  return (
    <div className="scrim" onClick={onClose}>
      <aside className="drawer" role="dialog" aria-label={`Run ${runId}`} onClick={(e) => e.stopPropagation()}>
        <header>
          <div className="row spread" style={{ marginBottom: 10 }}>
            <div className="row wrap">
              <h2>Run {runId}</h2>
              {d && <StatusChip status={d.run.status} />}
              {d && <span className="muted">{d.run.task_id}{blind ? "" : `, ${d.variant?.label ?? d.run.variant_key}, rep ${d.run.rep + 1}`}</span>}
            </div>
            <button className="btn" onClick={onClose}>Close<kbd>Esc</kbd></button>
          </div>
          <Tabs tabs={tabs} value={tab} onChange={setTab} />
        </header>
        <div className="scroll">
          <ErrorNote error={q.error} />
          {d && (
            <div className="stack">
              {d.run.error && <div className="note err">{d.run.error}</div>}
              {!blind && (d.run.events ?? []).length > 0 && (
                <div className="note">{d.run.events.map((e: any, i: number) => <div key={i}><b>{e.kind}</b> at {(e.at_ms / 1000).toFixed(1)}s: {e.detail}</div>)}</div>
              )}
              {tab === "output" && (
                <>
                  <p className="muted small">{d.meta?.output_is_file ? `Contents of ${d.meta.output_file}` : "The agent's final message"}</p>
                  <TextLines text={d.run.output_text} kind="output" annotations={blind ? [] : d.annotations} />
                </>
              )}
              {tab === "diff" && (d.diff ? <TextLines text={d.diff} kind="diff" annotations={blind ? [] : d.annotations} /> : <p className="muted">The agent changed no files.</p>)}
              {tab === "transcript" && <Transcript turns={d.transcript?.turns ?? []} />}
              {tab === "grades" && <Grades grades={d.grades} />}
              {tab === "metrics" && (
                <>
                  <div className="kv">
                    <div><span>Model</span><b>{d.run.model || "–"}</b></div>
                    <div><span>Skills invoked</span><b>{d.run.skills?.length ? d.run.skills.join(", ") : "none"}</b></div>
                    {Object.entries(d.metrics as Record<string, number>).filter(([k]) => !k.startsWith("tool.")).map(([k, v]) => <div key={k}><span>{info(k).label}</span><b>{fmtValue(info(k), v)}</b></div>)}
                  </div>
                  <h3>Tool calls</h3>
                  <div className="kv">
                    {Object.entries(d.metrics as Record<string, number>).filter(([k]) => k.startsWith("tool.")).map(([k, v]) => <div key={k}><span>{k.slice(5)}</span><b>{v}</b></div>)}
                  </div>
                  <p className="faint small">Artifacts: <code>{d.run.dir}</code></p>
                </>
              )}
            </div>
          )}
        </div>
      </aside>
    </div>
  );
}

function Grades({ grades }: { grades: any[] }) {
  if (!grades?.length) return <p className="muted">No graders ran on this run.</p>;
  return (
    <div className="stack">
      {grades.map((g) => (
        <div key={g.grader} className="stack tight">
          <div className="row">
            <span className={`chip ${g.passed === true ? "good" : g.passed === false ? "bad" : ""}`}>{g.passed === true ? "✓ pass" : g.passed === false ? "✗ fail" : "scored"}</span>
            <b>{g.grader}</b>
            <span className="muted small">{g.tier}{g.score !== null && g.score !== undefined ? `, value ${g.score}` : ""}</span>
          </div>
          {g.tier === "judge" ? (
            <>
              {g.detail?.summary && <p>{g.detail.summary}</p>}
              <table className="grid">
                <tbody>
                  {(g.detail?.expectations ?? []).map((e: any, i: number) => (
                    <tr key={i}>
                      <td style={{ width: 70 }}><span className={`chip ${e.passed ? "good" : "bad"}`}>{e.passed ? "✓ met" : "✗ not met"}</span></td>
                      <td><b>{e.text}</b><div className="muted">{e.evidence}</div></td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </>
          ) : <pre className="mono" style={{ margin: 0, whiteSpace: "pre-wrap", color: "var(--ink-2)" }}>{JSON.stringify(g.detail, null, 1)}</pre>}
        </div>
      ))}
    </div>
  );
}

function Transcript({ turns }: { turns: any[] }) {
  if (!turns.length) return <p className="muted">No transcript was captured.</p>;
  return (
    <div>
      {turns.map((t, i) => (
        <div key={i} className={`turn ${t.role}`}>
          <div className="who">{t.role}{t.subagent ? " (subagent)" : ""}</div>
          {t.text && <div className="text">{t.text}</div>}
          {(t.tool_calls ?? []).map((c: any) => (
            <details key={c.id}>
              <summary><b>{c.name}</b> <span className="muted small">{summarize(c.input)}</span></summary>
              <pre>{JSON.stringify(c.input, null, 1)}</pre>
            </details>
          ))}
          {(t.tool_results ?? []).map((r: any, j: number) => (
            <details key={j}>
              <summary className={r.is_error ? "" : "muted"}>{r.is_error ? "Tool error" : "Tool result"} <span className="small">{String(r.content).split("\n")[0].slice(0, 90)}</span></summary>
              <pre>{r.content}</pre>
            </details>
          ))}
        </div>
      ))}
    </div>
  );
}

function summarize(input: any): string {
  const v = input?.file_path ?? input?.command ?? input?.skill ?? input?.pattern ?? input?.description ?? "";
  const s = typeof v === "string" ? v : JSON.stringify(v);
  return s.length > 90 ? `…${s.slice(-88)}` : s;
}
