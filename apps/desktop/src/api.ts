// One command surface, two transports: Tauri `invoke` inside the desktop
// app, HTTP against `wrangler serve` in a browser.

export const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

export async function call<T = any>(cmd: string, args: Record<string, unknown> = {}): Promise<T> {
  if (inTauri) {
    const { invoke } = await import("@tauri-apps/api/core");
    return invoke<T>("api", { cmd, args });
  }
  const res = await fetch(`/api/${cmd}`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(args) });
  const body = await res.json().catch(() => ({ ok: false, error: `HTTP ${res.status}` }));
  if (!body.ok) throw new Error(body.error || "request failed");
  return body.data as T;
}

/** Desktop only: project selection lives in the Tauri shell. */
export async function shell<T = any>(cmd: string, args: Record<string, unknown> = {}): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(cmd, args);
}

/** Folder the picker starts in: beside the open project, else beside the most recent one. */
async function pickerStart(): Promise<string | undefined> {
  const current = await shell<string | null>("project_current").catch(() => null);
  const recent = current ? [] : await shell<string[]>("recent_projects").catch(() => []);
  const near = current ?? recent[0];
  return near ? near.replace(/\/[^/]+\/?$/, "") || "/" : undefined;
}

export async function pickFolder(): Promise<string | null> {
  // Scripted runs set WRANGLER_PICK_FOLDER to skip the system picker.
  const scripted = await shell<string | null>("pick_override").catch(() => null);
  if (scripted) return scripted;
  const { open } = await import("@tauri-apps/plugin-dialog");
  const r = await open({ directory: true, multiple: false, title: "Choose a Harness Wrangler project folder", defaultPath: await pickerStart() });
  return typeof r === "string" ? r : null;
}

export interface MetricInfo { name: string; label: string; direction: "higher" | "lower" | "none"; scale: "rate" | "score10" | "positive"; unit: string }
export interface Summary { n: number; mean: number; sd: number; min: number; max: number; ci_low: number; ci_high: number }
export type Verdict = "better" | "worse" | "higher" | "lower" | "neutral" | "inconclusive";
export interface Effect { effect: number; ci_low: number; ci_high: number; rel: number | null; base_mean: number; treat_mean: number; units: number; n_a: number; n_b: number; verdict: Verdict }
export interface Comparison extends Effect { kind: "factor" | "variant"; factor: string | null; baseline: string; level: string; variant_key: string | null; metric: string }
export interface Contrast { diff: number; ci_low: number; ci_high: number; differs: boolean }
export interface Conditional extends Effect { factor: string; baseline: string; level: string; by_factor: string; by_level: string; metric: string; vs_first: Contrast | null; first_level: string }
export interface Counts { total: number; done: number; failed: number; timeout: number; pending: number; running: number; cancelled: number }
export interface VariantSummary { key: string; label: string; levels: Record<string, string>; runner: string; model: string | null; effort: string | null; counts: Counts; metrics: Record<string, Summary> }
export interface Cell { variant_key: string; counts: Counts; metrics: Record<string, Summary>; flaky: string[]; run_ids: number[] }
export interface Report {
  launch_id: number; experiment: string; description: string; status: string; phase: string; created_at: string; def_hash: string; runs_per_cell: number;
  counts: Counts; cost_usd: number; judge_cost_usd: number; primary_metric: string; metrics: MetricInfo[];
  factors: { name: string; kind: string; levels: string[] }[]; variants: VariantSummary[]; comparisons: Comparison[]; conditional: Conditional[];
  tasks: { task_id: string; tags: string[]; cells: Cell[] }[]; flaky: any[]; pairwise: any | null; ablation: any | null; events: any; warnings: string[];
}
export interface LaunchSummary { id: number; experiment: string; status: string; phase: string; created_at: string; finished_at: string | null; suite: string | null; note: string; runs_total: number; runs_done: number; runs_failed: number; cost_usd: number }

const KNOWN: Record<string, Partial<MetricInfo>> = {};
export function remember(metrics: MetricInfo[]) { for (const m of metrics) KNOWN[m.name] = m; }
export function info(name: string): MetricInfo {
  const k = KNOWN[name];
  if (k) return k as MetricInfo;
  const fallback: Record<string, MetricInfo> = {
    judge_score: { name, label: "Judge score", direction: "higher", scale: "score10", unit: "/10" },
    pass: { name, label: "Pass rate", direction: "higher", scale: "rate", unit: "" },
    cost_usd: { name, label: "Cost", direction: "lower", scale: "positive", unit: "$" },
    wall_time_s: { name, label: "Wall time", direction: "lower", scale: "positive", unit: "s" },
    total_tokens: { name, label: "Total tokens", direction: "lower", scale: "positive", unit: "tok" },
  };
  if (name.startsWith("check.")) return { name, label: `Check: ${name.slice(6)}`, direction: "higher", scale: "rate", unit: "" };
  return fallback[name] ?? { name, label: name, direction: "none", scale: "positive", unit: "" };
}

const MINUS = "−";
export function fmtValue(m: MetricInfo, v: number | undefined | null): string {
  if (v === undefined || v === null || Number.isNaN(v)) return "–";
  if (m.unit === "$") return `$${v.toFixed(v < 1 ? 4 : 2)}`;
  if (m.scale === "rate") return `${Math.round(v * 100)}%`;
  if (m.scale === "score10") return v.toFixed(2);
  if (m.unit === "tok") return `${(v / 1000).toFixed(1)}k`;
  if (m.unit === "s") return `${v.toFixed(1)}s`;
  return v.toFixed(1);
}
export function fmtDelta(m: MetricInfo, v: number | undefined | null): string {
  if (v === undefined || v === null || Number.isNaN(v)) return "–";
  const a = Math.abs(v);
  const sign = v >= 0 ? "+" : MINUS;
  let body: string;
  if (m.unit === "$") body = `$${a.toFixed(a < 1 ? 4 : 2)}`;
  else if (m.scale === "rate") body = `${Math.round(a * 100)}pp`;
  else if (m.scale === "score10") body = a.toFixed(2);
  else if (m.unit === "tok") body = `${(a / 1000).toFixed(1)}k`;
  else if (m.unit === "s") body = `${a.toFixed(1)}s`;
  else body = a.toFixed(1);
  return sign + body;
}
export function fmtRel(rel: number | null | undefined): string {
  if (rel === null || rel === undefined || !Number.isFinite(rel)) return "";
  return `${rel >= 0 ? "+" : MINUS}${Math.abs(rel * 100).toFixed(0)}%`;
}
export function usd(v: number): string { return `$${v.toFixed(v < 1 ? 4 : 2)}`; }
export function when(iso: string | null | undefined): string {
  if (!iso) return "";
  const d = new Date(iso);
  return d.toLocaleString(undefined, { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
}
