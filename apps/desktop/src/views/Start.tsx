import type { Ctx } from "../App";
import { ErrorNote } from "../components";
import { useLoad } from "../hooks";

/**
 * Where the project stands and what to do next. The steps run from an empty
 * folder to acting on results; the first one not yet done is the next step.
 */
export default function Start({ ctx, onAgent }: { ctx: Ctx; onAgent?: () => void }) {
  const q = useLoad<any>("guide", {}, [ctx.launches.length, ctx.launches[0]?.status, ctx.launches[0]?.runs_done], 5000);
  const g = q.data;
  return (
    <div className="content">
      <div className="page" style={{ maxWidth: 860 }}>
        <div className="stack tight">
          <h1>{g ? `Getting an answer from ${g.project}` : "Getting started"}</h1>
          <p className="muted" style={{ maxWidth: "74ch" }}>
            Harness Wrangler runs the same tasks with and without something you want to test (a skill, a CLAUDE.md, a model, an effort level), many times,
            and tells you whether it helped, by how much, and what to change. These are the steps; the highlighted one is next.
          </p>
        </div>
        <ErrorNote error={q.error} />
        {g && (
          <ol className="steps">
            {g.steps.map((s: any, i: number) => {
              const state = s.done ? "done" : s.active ? "active" : i === g.next ? "next" : "later";
              return (
                <li key={s.id} className={`step ${state}`} aria-current={i === g.next ? "step" : undefined}>
                  <span className="mark" aria-hidden>{s.done ? "✓" : i + 1}</span>
                  <div className="stack tight">
                    <div className="row wrap">
                      <h2>{s.title}</h2>
                      {s.done && <span className="chip good">done</span>}
                      {s.active && <span className="chip accent">running</span>}
                      {!s.done && !s.active && i === g.next && <span className="chip warn">next</span>}
                      {s.optional && <span className="chip">optional</span>}
                    </div>
                    <p>{s.detail}</p>
                    <p className="faint small">Takes {s.time}.</p>
                    {(s.action || (s.id === "tasks" && onAgent)) && (
                      <div className="row wrap">
                        {s.action && <button className={`btn ${i === g.next || s.active ? "primary" : ""}`} onClick={() => ctx.go(s.action.href)}>{s.action.label}</button>}
                        {s.action?.second && <button className="btn" onClick={() => ctx.go(s.action.second.href)}>{s.action.second.label}</button>}
                        {s.id === "tasks" && onAgent && <button className="btn" onClick={onAgent}>Ask the agent to draft tasks</button>}
                      </div>
                    )}
                  </div>
                </li>
              );
            })}
          </ol>
        )}
        <div className="note">
          Other things you can do once you have results: find which sections of a skill carry its effect (Ablation), and track a set of skills across model releases (Regression).
          {onAgent ? " The agent in the terminal knows this tool's file formats and commands; ask it for help at any point." : ""}
        </div>
      </div>
    </div>
  );
}
