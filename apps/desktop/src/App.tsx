import { useEffect, useMemo, useState } from "react";
import { LaunchSummary, call, inTauri, pickFolder, shell, usd, when } from "./api";
import { ErrorNote, StatusChip } from "./components";
import { useLoad, useRoute } from "./hooks";
import Ablation from "./views/Ablation";
import Advice from "./views/Advice";
import Experiments from "./views/Experiments";
import Regression from "./views/Regression";
import Results from "./views/Results";
import Review from "./views/Review";
import RunDrawer from "./views/RunDrawer";

const NAV: [string, string][] = [
  ["experiments", "Experiments"],
  ["results", "Results"],
  ["review", "Review"],
  ["ablation", "Ablation"],
  ["regression", "Regression"],
  ["advice", "Advice"],
];

export interface Ctx {
  launches: LaunchSummary[];
  launchId: number | null;
  go: (path: string) => void;
  openRun: (id: number, blind?: boolean) => void;
  reloadLaunches: () => void;
}

/** Desktop only: pick or create the project folder before anything else. */
function ProjectGate({ onReady }: { onReady: () => void }) {
  const [error, setError] = useState<string | null>(null);
  const [recent, setRecent] = useState<string[]>([]);
  const [pending, setPending] = useState<string | null>(null);
  useEffect(() => {
    shell<string[]>("recent_projects").then(setRecent).catch(() => {});
    shell<string | null>("project_pending").then(setPending).catch(() => {});
  }, []);
  const open = async (path: string | null, init = false) => {
    if (!path) return;
    try {
      await shell(init ? "project_init" : "project_open", { path });
      onReady();
    } catch (e: any) { setError(String(e.message ?? e)); }
  };
  return (
    <div className="content">
      <div className="page" style={{ maxWidth: 620, margin: "8vh auto" }}>
        <h1>Open a project</h1>
        <p className="muted">A project is a folder you own, usually a git repo, holding experiments, tasks and overlays as files. Results stay on this machine in its <code>.wrangler</code> folder.</p>
        <div className="row">
          <button className="btn primary" onClick={async () => open(await pickFolder())}>Open project folder</button>
          <button className="btn" onClick={async () => open(await pickFolder(), true)}>Create project in a folder</button>
        </div>
        <ErrorNote error={error} />
        {pending && <div className="note">Opening {pending}… If macOS asks whether Harness Wrangler may access this folder, choose Allow.</div>}
        {recent.length > 0 && (
          <section className="panel">
            <header><h2>Recent</h2></header>
            <div className="list">{recent.map((p) => <button key={p} onClick={() => open(p)}><b>{p.split("/").pop()}</b><span>{p}</span></button>)}</div>
          </section>
        )}
      </div>
    </div>
  );
}

export default function App() {
  const [parts, go] = useRoute();
  const [ready, setReady] = useState(!inTauri);
  const [projectKey, setProjectKey] = useState(0);
  const [run, setRun] = useState<{ id: number; blind: boolean } | null>(null);
  useEffect(() => {
    if (inTauri) shell<string | null>("project_current").then((p) => setReady(!!p)).catch(() => setReady(false));
  }, []);

  // File menu: Open Project, New Project, Close Project, Open Recent.
  useEffect(() => {
    if (!inTauri) return;
    let off: (() => void)[] = [];
    let gone = false;
    const reopen = () => { setMenuError(null); setRun(null); setReady(true); setProjectKey((k) => k + 1); window.location.hash = "/experiments"; };
    import("@tauri-apps/api/event").then(async ({ listen }) => {
      const a = await listen<string>("project-menu", async (e) => {
        if (e.payload === "project-close") { await shell("project_close"); setRun(null); setReady(false); return; }
        const path = await pickFolder();
        if (!path) return;
        try { await shell(e.payload === "project-new" ? "project_init" : "project_open", { path }); reopen(); }
        catch (err: any) { setMenuError(String(err.message ?? err)); }
      });
      const b = await listen<string | null>("project-changed", (e) => { if (e.payload) setMenuError(e.payload); else reopen(); });
      if (gone) { a(); b(); } else off = [a, b];
    });
    return () => { gone = true; off.forEach((f) => f()); };
  }, []);
  const [menuError, setMenuError] = useState<string | null>(null);

  const project = useLoad(ready ? "project_info" : null, {}, [projectKey]);
  const launchesQ = useLoad<LaunchSummary[]>(ready ? "launches_list" : null, {}, [projectKey], 4000);
  const launches = launchesQ.data ?? [];
  const view = NAV.some(([id]) => id === parts[0]) ? parts[0] : "experiments";

  // The launch in view: from the URL, else the most recent one.
  const routeLaunch = ["results", "review", "ablation", "advice"].includes(view) && parts[1] ? Number(parts[1]) : null;
  const [sticky, setSticky] = useState<number | null>(null);
  useEffect(() => { if (routeLaunch) setSticky(routeLaunch); }, [routeLaunch]);
  const launchId = routeLaunch ?? sticky ?? launches[0]?.id ?? null;
  const current = launches.find((l) => l.id === launchId);

  const ctx: Ctx = useMemo(() => ({
    launches, launchId, go, reloadLaunches: launchesQ.reload,
    openRun: (id: number, blind = false) => setRun({ id, blind }),
  }), [launches, launchId, go, launchesQ.reload]);

  const [reviewLeft, setReviewLeft] = useState<number | null>(null);
  useEffect(() => {
    if (!launchId || !ready) return;
    let live = true;
    call("review_next", { launch_id: launchId }).then((r) => { if (live) setReviewLeft(r.progress?.useful_remaining ?? null); }).catch(() => setReviewLeft(null));
    return () => { live = false; };
  }, [launchId, ready, view]);

  if (!ready) return <div className="app" style={{ gridTemplateColumns: "1fr" }}>{menuError && <div style={{ padding: "16px 24px 0" }}><ErrorNote error={menuError} /></div>}<ProjectGate key={menuError ?? ""} onReady={() => { setReady(true); setProjectKey((k) => k + 1); }} /></div>;

  const perLaunch = ["results", "review", "ablation", "advice"].includes(view);
  const href = (id: string) => (["results", "review", "ablation", "advice"].includes(id) && launchId ? `#/${id}/${launchId}` : `#/${id}`);

  return (
    <div className="app">
      <nav className="rail" aria-label="Sections">
        <div className="brand">
          <b>Harness Wrangler</b>
          <span title={project.data?.root}>{project.data?.name ?? ""}</span>
        </div>
        {NAV.map(([id, label]) => (
          <a key={id} href={href(id)} className={view === id ? "on" : ""} aria-current={view === id ? "page" : undefined}>
            {label}
            {id === "review" && reviewLeft ? <span className="badge" title={`${reviewLeft} pairs worth reviewing in this launch`}>{reviewLeft > 99 ? "99+" : reviewLeft}</span> : null}
          </a>
        ))}
        <div className="foot">
          {inTauri && <button className="btn quiet sm" onClick={async () => { await shell("project_close"); setReady(false); }}>Switch project</button>}
          <div>v{project.data?.version ?? ""}</div>
        </div>
      </nav>
      <div className="main">
        {perLaunch && (
          <div className="bar">
            <label className="row small muted" style={{ gap: 6 }}>
              Launch
              <select value={launchId ?? ""} onChange={(e) => go(`/${view}/${e.target.value}`)} style={{ maxWidth: 420 }}>
                {launches.length === 0 && <option value="">No launches yet</option>}
                {launches.map((l) => (
                  <option key={l.id} value={l.id}>#{l.id} {l.experiment} ({when(l.created_at)}){l.note ? `: ${l.note}` : ""}</option>
                ))}
              </select>
            </label>
            {current && (
              <>
                <StatusChip status={current.status} phase={current.phase} />
                <span className="muted small">{current.runs_done}/{current.runs_total} runs, {usd(current.cost_usd)}</span>
              </>
            )}
            <span className="grow" />
            {current && <a className="small" href={`#/experiments/${encodeURIComponent(current.experiment)}`}>Open experiment</a>}
          </div>
        )}
        <ErrorNote error={menuError ?? project.error} />
        {view === "experiments" && <Experiments ctx={ctx} selected={parts[1] ?? null} sub={parts[2] ?? null} />}
        {view === "results" && <Results ctx={ctx} />}
        {view === "review" && <Review ctx={ctx} />}
        {view === "ablation" && <Ablation ctx={ctx} />}
        {view === "regression" && <Regression ctx={ctx} selected={parts[1] ?? null} />}
        {view === "advice" && <Advice ctx={ctx} />}
      </div>
      {run && <RunDrawer runId={run.id} blind={run.blind} onClose={() => setRun(null)} />}
    </div>
  );
}
