import { useEffect, useRef, useState } from "react";
import { call } from "../api";

export interface Annotation { id: number; run_id: number; kind: "comment" | "emoji"; target: string; line_start: number; line_end: number; col_start: number | null; col_end: number | null; quote: string; body: string }

const EMOJI = ["👍", "👎", "❓", "✂️", "🎯"];
const EMOJI_HINT: Record<string, string> = { "👍": "Good", "👎": "Bad", "❓": "Unclear", "✂️": "Cut this", "🎯": "Exactly right" };

interface Pending { line_start: number; line_end: number; col_start: number | null; col_end: number | null; quote: string; x: number; y: number }

function diffClass(line: string): string {
  if (line.startsWith("diff --git") || line.startsWith("+++") || line.startsWith("---")) return "file";
  if (line.startsWith("@@")) return "hunk";
  if (line.startsWith("+")) return "add";
  if (line.startsWith("-")) return "del";
  return "";
}

/**
 * Line-numbered text (an output or a diff) with inline annotations. When
 * `runId` and `onChange` are given, selecting text or a line number opens a
 * small bar for an emoji reaction or a comment anchored to that range.
 */
export function TextLines({ text, kind, annotations, runId, onChange }: { text: string; kind: "output" | "diff"; annotations: Annotation[]; runId?: number; onChange?: (all: Annotation[]) => void }) {
  const root = useRef<HTMLDivElement>(null);
  const [pending, setPending] = useState<Pending | null>(null);
  const [comment, setComment] = useState("");
  const editable = runId !== undefined && !!onChange;
  const lines = text.replace(/\n$/, "").split("\n");
  const mine = annotations.filter((a) => a.target === kind);

  useEffect(() => { setPending(null); setComment(""); }, [text, runId]);
  useEffect(() => {
    if (!pending) return;
    const close = (e: MouseEvent) => { if (!(e.target as HTMLElement).closest(".selbar")) setPending(null); };
    const esc = (e: KeyboardEvent) => { if (e.key === "Escape") setPending(null); };
    window.addEventListener("mousedown", close);
    window.addEventListener("keydown", esc);
    return () => { window.removeEventListener("mousedown", close); window.removeEventListener("keydown", esc); };
  }, [pending]);

  const lineOf = (node: Node | null): HTMLElement | null => {
    const el = node instanceof HTMLElement ? node : node?.parentElement;
    return el?.closest<HTMLElement>("[data-line]") ?? null;
  };
  const offsetIn = (lineEl: HTMLElement, node: Node, offset: number): number => {
    const r = document.createRange();
    r.selectNodeContents(lineEl.querySelector(".lt") ?? lineEl);
    try { r.setEnd(node, offset); } catch { return 0; }
    return r.toString().length;
  };

  const onMouseUp = () => {
    if (!editable) return;
    const sel = window.getSelection();
    if (!sel || sel.isCollapsed || sel.rangeCount === 0) return;
    const range = sel.getRangeAt(0);
    const a = lineOf(range.startContainer);
    const b = lineOf(range.endContainer);
    if (!a || !b || !root.current?.contains(a) || !root.current.contains(b)) return;
    const quote = sel.toString().trim();
    if (!quote) return;
    const rect = range.getBoundingClientRect();
    const ls = Number(a.dataset.line);
    const le = Number(b.dataset.line);
    setComment("");
    setPending({
      line_start: ls, line_end: le,
      col_start: offsetIn(a, range.startContainer, range.startOffset),
      col_end: offsetIn(b, range.endContainer, range.endOffset),
      quote: quote.slice(0, 600),
      x: Math.min(rect.left, window.innerWidth - 470), y: rect.bottom + 6,
    });
  };

  const pickLine = (n: number, ev: React.MouseEvent) => {
    if (!editable) return;
    const r = (ev.currentTarget as HTMLElement).getBoundingClientRect();
    setComment("");
    setPending({ line_start: n, line_end: n, col_start: null, col_end: null, quote: lines[n - 1].trim().slice(0, 600), x: r.right + 4, y: r.bottom + 4 });
  };

  const add = async (k: "comment" | "emoji", body: string) => {
    if (!pending || !editable || !body.trim()) return;
    const { x: _x, y: _y, ...anchor } = pending;
    const res = await call("annotation_add", { run_id: runId, kind: k, target: kind, body: body.trim(), ...anchor });
    onChange!(res.annotations);
    setPending(null);
    setComment("");
    window.getSelection()?.removeAllRanges();
  };
  const remove = async (id: number) => {
    await call("annotation_delete", { id });
    onChange!(annotations.filter((a) => a.id !== id));
  };

  return (
    <div ref={root} className={`lines ${kind === "output" ? "prose" : ""}`} onMouseUp={onMouseUp}>
      {lines.map((line, i) => {
        const n = i + 1;
        const covering = mine.filter((a) => a.line_start <= n && n <= a.line_end);
        const ending = mine.filter((a) => a.line_end === n);
        // Mark the exact span when an annotation sits within this one line.
        const span = covering.find((a) => a.line_start === a.line_end && a.col_start !== null && a.col_end !== null && a.col_end! > a.col_start!);
        const body = span
          ? <>{line.slice(0, span.col_start!)}<mark>{line.slice(span.col_start!, span.col_end!)}</mark>{line.slice(span.col_end!)}</>
          : covering.length > 0 ? <mark>{line || " "}</mark> : line || " ";
        const reactions = ending.filter((a) => a.kind === "emoji");
        return (
          <div key={n}>
            <div className={`ln ${kind === "diff" ? diffClass(line) : ""} ${covering.length ? "noted" : ""}`} data-line={n}>
              <button tabIndex={editable ? 0 : -1} aria-label={editable ? `Annotate line ${n}` : `Line ${n}`} onClick={(e) => pickLine(n, e)}>{n}</button>
              <span className="lt">{body}</span>
            </div>
            {reactions.length > 0 && (
              <div className="emoji-row">
                {reactions.map((a) => <button key={a.id} title={editable ? `${EMOJI_HINT[a.body] ?? ""}: "${a.quote}". Select to remove.` : a.quote} onClick={() => editable && remove(a.id)}>{a.body}</button>)}
              </div>
            )}
            {ending.filter((a) => a.kind === "comment").map((a) => (
              <div key={a.id} className="margin-note">
                <span>{a.body}{a.line_start !== a.line_end && <span className="faint small"> (lines {a.line_start}–{a.line_end})</span>}</span>
                {editable && <button aria-label="Delete comment" onClick={() => remove(a.id)}>×</button>}
              </div>
            ))}
          </div>
        );
      })}
      {pending && (
        <div className="selbar" style={{ left: Math.max(8, pending.x), top: Math.min(pending.y, window.innerHeight - 60) }} role="dialog" aria-label="Annotate selection">
          {EMOJI.map((e) => <button key={e} title={EMOJI_HINT[e]} onClick={() => add("emoji", e)}>{e}</button>)}
          <input autoFocus placeholder="Comment, then Enter" value={comment} onChange={(e) => setComment(e.target.value)}
            onKeyDown={(e) => { e.stopPropagation(); if (e.key === "Enter") add("comment", comment); if (e.key === "Escape") setPending(null); }} />
        </div>
      )}
    </div>
  );
}
