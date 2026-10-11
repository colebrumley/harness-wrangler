import { ReactNode, useState } from "react";
import { Effect, MetricInfo, Verdict, fmtDelta, fmtRel, fmtValue } from "./api";

export function ErrorNote({ error }: { error: string | null | undefined }) {
  if (!error) return null;
  return <div className="note err" role="alert">{error}</div>;
}

export function Empty({ title, children }: { title: string; children?: ReactNode }) {
  return (
    <div className="empty">
      <h2>{title}</h2>
      <div>{children}</div>
    </div>
  );
}

export function Panel({ title, sub, actions, children, flush }: { title?: ReactNode; sub?: ReactNode; actions?: ReactNode; children: ReactNode; flush?: boolean }) {
  return (
    <section className="panel">
      {(title || actions) && (
        <header>
          <h2>{title}</h2>
          {sub && <p className="grow">{sub}</p>}
          {!sub && <span className="grow" />}
          {actions}
        </header>
      )}
      <div className={flush ? "body flushx" : "body"}>{children}</div>
    </section>
  );
}

export function Tabs<T extends string>({ tabs, value, onChange }: { tabs: [T, string][]; value: T; onChange: (t: T) => void }) {
  return (
    <div className="tabs" role="tablist">
      {tabs.map(([id, label]) => (
        <button key={id} role="tab" aria-selected={value === id} className={value === id ? "on" : ""} onClick={() => onChange(id)}>{label}</button>
      ))}
    </div>
  );
}

export function Seg<T extends string>({ options, value, onChange }: { options: [T, string][]; value: T; onChange: (t: T) => void }) {
  return (
    <span className="seg">
      {options.map(([id, label]) => (
        <button key={id} className={value === id ? "on" : ""} onClick={() => onChange(id)}>{label}</button>
      ))}
    </span>
  );
}

const STATUS_CHIP: Record<string, string> = { done: "good", running: "accent", failed: "bad", timeout: "warn", cancelled: "", interrupted: "warn", budget: "warn", pending: "" };
export function StatusChip({ status, phase }: { status: string; phase?: string }) {
  const word = status === "budget" ? "budget reached" : status;
  return <span className={`chip ${STATUS_CHIP[status] ?? ""}`}>{word}{phase ? `: ${phase}` : ""}</span>;
}

// Verdicts never rely on colour alone: each has its own glyph and word.
const GLYPH: Record<Verdict, string> = { better: "▲", worse: "▼", higher: "△", lower: "▽", neutral: "●", inconclusive: "○" };
const WORD: Record<Verdict, string> = { better: "better", worse: "worse", higher: "higher", lower: "lower", neutral: "no effect", inconclusive: "inconclusive" };

export function VerdictMark({ verdict, word }: { verdict: Verdict; word?: string }) {
  return (
    <span className={`verdict v-${verdict}`}>
      <i aria-hidden>{GLYPH[verdict]}</i>
      {word ?? WORD[verdict]}
    </span>
  );
}

export interface ForestRow { key: string; label: ReactNode; sub?: ReactNode; effect: Effect | null | undefined; word?: string; onClick?: () => void }

interface Tip { x: number; y: number; body: ReactNode }

/**
 * Effect sizes with 95% CIs on one shared axis through zero. The shaded
 * band is the "no meaningful effect" zone; a whisker that crosses zero is
 * inconclusive unless it sits inside the band.
 */
export function Forest({ rows, metric, labelHead, band }: { rows: ForestRow[]; metric: MetricInfo; labelHead: string; band?: number }) {
  const [tip, setTip] = useState<Tip | null>(null);
  const effects = rows.map((r) => r.effect).filter(Boolean) as Effect[];
  if (effects.length === 0) return <p className="muted">No comparison data yet for {metric.label.toLowerCase()}.</p>;
  const reach = Math.max(...effects.flatMap((e) => [Math.abs(e.ci_low), Math.abs(e.ci_high), Math.abs(e.effect)]), band ?? 0, 1e-9) * 1.08;
  const x = (v: number) => 50 + (v / reach) * 50;
  const better = metric.direction === "higher" ? "right" : metric.direction === "lower" ? "left" : null;
  return (
    <div className="forest" style={{ gridTemplateColumns: "minmax(150px, 240px) minmax(180px, 1fr) 200px 112px" }}>
      <div className="fh">{labelHead}</div>
      <div className="fh">
        <div className="axis">
          <span>{fmtDelta(metric, -reach)}{better === "left" ? " (better)" : ""}</span>
          <span>0</span>
          <span>{better === "right" ? "(better) " : ""}{fmtDelta(metric, reach)}</span>
        </div>
      </div>
      <div className="fh num">Effect, 95% CI</div>
      <div className="fh">Verdict</div>
      {rows.map((r) => {
        const e = r.effect;
        return [
          <div key={`${r.key}-l`} className="fl clip" title={typeof r.label === "string" ? r.label : undefined}>
            {r.onClick ? <button className="btn quiet" style={{ padding: 0 }} onClick={r.onClick}>{r.label}</button> : r.label}
            {r.sub && <small>{r.sub}</small>}
          </div>,
          <div key={`${r.key}-p`} className={`fplot ${e ? `v-${e.verdict}` : ""}`}
            onMouseMove={(ev) => e && setTip({ x: ev.clientX + 14, y: ev.clientY + 14, body: (
              <>
                <b>{r.label}</b><br />
                {metric.label}: {fmtValue(metric, e.base_mean)} → {fmtValue(metric, e.treat_mean)}<br />
                Effect {fmtDelta(metric, e.effect)} {fmtRel(e.rel) && `(${fmtRel(e.rel)})`}<br />
                95% CI {fmtDelta(metric, e.ci_low)} to {fmtDelta(metric, e.ci_high)}<br />
                {e.units} paired unit{e.units === 1 ? "" : "s"}, {e.n_a} vs {e.n_b} runs
              </>
            ) })}
            onMouseLeave={() => setTip(null)}>
            <svg viewBox="0 0 100 30" preserveAspectRatio="none" role="img" aria-label={e ? `${fmtDelta(metric, e.effect)}, CI ${fmtDelta(metric, e.ci_low)} to ${fmtDelta(metric, e.ci_high)}` : "no data"}>
              {band ? <rect className="band" x={x(-band)} width={x(band) - x(-band)} y="0" height="30" /> : null}
              <line className="zero" x1="50" x2="50" y1="0" y2="30" vectorEffect="non-scaling-stroke" />
              {e && <line className="whisker" x1={x(e.ci_low)} x2={x(e.ci_high)} y1="15" y2="15" vectorEffect="non-scaling-stroke" />}
            </svg>
            {e && <span className="fdot" style={{ left: `${x(e.effect)}%` }} />}
          </div>,
          <div key={`${r.key}-v`} className="fv num">
            {e ? <><b>{fmtDelta(metric, e.effect)}</b> <span>[{fmtDelta(metric, e.ci_low)}, {fmtDelta(metric, e.ci_high)}]</span></> : <span>no data</span>}
          </div>,
          <div key={`${r.key}-w`}>{e ? <VerdictMark verdict={e.verdict} word={r.word} /> : null}</div>,
          <div key={`${r.key}-s`} className="sep" />,
        ];
      })}
      {tip && <div className="tip" style={{ left: Math.min(tip.x, window.innerWidth - 340), top: tip.y }}>{tip.body}</div>}
    </div>
  );
}

/** Mean with its bootstrapped CI as a thin bar, scaled to `max`. */
export function MeanBar({ mean, lo, hi, max }: { mean: number; lo: number; hi: number; max: number }) {
  const pct = (v: number) => `${Math.max(0, Math.min(100, (v / (max || 1)) * 100))}%`;
  return (
    <div className="meanbar" aria-hidden>
      <div className="fill" style={{ width: pct(mean) }} />
      <div className="ci" style={{ left: pct(lo), width: `calc(${pct(hi)} - ${pct(lo)})` }} />
    </div>
  );
}

export function equivalenceBand(metric: MetricInfo, baseMean: number): number {
  if (metric.scale === "rate") return 0.1;
  if (metric.scale === "score10") return 0.5;
  return Math.abs(baseMean) * 0.1;
}
