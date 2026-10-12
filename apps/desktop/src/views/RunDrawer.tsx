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
  // A grade that names tool calls links to them: open the transcript at that call.
  const [focusCall, setFocusCall] = useState<string | null>(null);
  const showCall = (id: string) => { setFocusCall(id); setTab("transcript"); };
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
              {tab === "transcript" && <Transcript turns={d.transcript?.turns ?? []} focusCall={focusCall} />}
              {tab === "grades" && <Grades grades={d.grades} onCall={showCall} />}
              {tab === "metrics" && (
                <>
                  <div className="kv">
                    <div><span>Model</span><b>{d.run.model || "–"}</b></div>
                    <div><span>Skills invoked</span><b>{d.run.skills?.length ? d.run.skills.join(", ") : "none"}</b></div>
                    {Object.entries(d.metrics as Record<string, number>).filter(([k]) => !isToolMetric(k)).map(([k, v]) => <div key={k}><span>{info(k).label}</span><b>{fmtValue(info(k), v)}</b></div>)}
                  </div>
                  <h3>Tool calls</h3>
                  <div className="kv">
                    {Object.entries(d.metrics as Record<string, number>).filter(([k]) => isToolMetric(k)).map(([k, v]) => <div key={k}><span>{k.startsWith("tool.") ? k.slice(5) : k}</span><b>{v}</b></div>)}
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

/** Per-tool and per-hook counts: `tool.<Name>`, `mcp.<server>.<tool>`, `hook.<Event>`. */
function isToolMetric(k: string): boolean {
  return k.startsWith("tool.") || k.startsWith("mcp.") || k.startsWith("hook.");
}

/** What a transcript grader matched: the calls, each a link to its turn, and the step that was missing. */
function MatchedCalls({ detail, onCall }: { detail: any; onCall: (id: string) => void }) {
  const calls: any[] = detail?.calls ?? [];
  const missing = detail?.missing_step;
  const hooks: any[] = detail?.hooks ?? [];
  return (
    <div className="stack tight">
      {detail?.error && <div className="note err">{detail.error}</div>}
      {detail?.count !== undefined && <span className="muted small">{detail.count} matching {detail.event ? `${detail.event} hook run` : "call"}{detail.count === 1 ? "" : "s"}{detail.min != null ? `, at least ${detail.min} wanted` : ""}{detail.max != null ? `, at most ${detail.max}` : ""}</span>}
      {detail?.steps !== undefined && <span className="muted small">{detail.matched_steps} of {detail.steps} steps matched{detail.before_final ? ", before the final message" : ""}</span>}
      {calls.map((c, i) => (
        <div key={c.id ?? i} className="row">
          <button className="btn sm" onClick={() => onCall(c.id)} title="Show this call in the transcript">turn {c.turn + 1}</button>
          <b>{c.name}</b> <code className="small">{c.input}</code>
        </div>
      ))}
      {missing && <div className="note warn">Step {missing.index} ({(missing.tool ?? []).join(" or ")}{missing.input ? ` matching /${missing.input}/` : ""}): {missing.why}.</div>}
      {hooks.map((h, i) => <div key={i} className="small"><b>{h.event}</b>{h.matcher ? ` [${h.matcher}]` : ""} exit {h.exit_code ?? "?"} <code>{String(h.output ?? "").slice(0, 120)}</code></div>)}
      {detail?.matched !== undefined && detail?.scope && <span className="muted small">{detail.matched ? <>matched <code>{detail.matched}</code> in turn {detail.turn + 1} ({detail.scope})</> : `no match in ${detail.scope}`}</span>}
    </div>
  );
}

const TRANSCRIPT_GRADERS = ["call_ids", "missing_step", "hooks", "scope"];

function Grades({ grades, onCall }: { grades: any[]; onCall: (id: string) => void }) {
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
          ) : TRANSCRIPT_GRADERS.some((k) => g.detail && k in g.detail) || (g.detail?.error && g.detail?.event)
            ? <MatchedCalls detail={g.detail} onCall={onCall} />
            : <pre className="mono" style={{ margin: 0, whiteSpace: "pre-wrap", color: "var(--ink-2)" }}>{JSON.stringify(g.detail, null, 1)}</pre>}
        </div>
      ))}
    </div>
  );
}

export function Transcript({ turns, focusCall }: { turns: any[]; focusCall?: string | null }) {
  useEffect(() => {
    if (focusCall) document.getElementById(`call-${focusCall}`)?.scrollIntoView({ block: "center" });
  }, [focusCall]);
  if (!turns.length) return <p className="muted">No transcript was captured.</p>;
  return (
    <div>
      {turns.map((t, i) => (
        <div key={i} className={`turn ${t.role}${t.simulated ? " simulated" : ""}`}>
          <div className="who">{t.simulated ? "user (simulated)" : t.role}{t.subagent ? " (subagent)" : ""}</div>
          {t.text && <div className="text">{t.text}</div>}
          {(t.tool_calls ?? []).map((c: any) => (
            <details key={c.id} id={`call-${c.id}`} open={c.id === focusCall} className={c.id === focusCall ? "focus" : ""}>
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
