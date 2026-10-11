import { useCallback, useEffect, useRef, useState } from "react";
import type { Ctx } from "../App";
import { call } from "../api";
import { Empty, ErrorNote, Seg } from "../components";
import { Annotation, TextLines } from "./TextLines";

const WHY: Record<string, string> = {
  undecided_comparison: "this comparison is still undecided",
  task_unreviewed: "no one has reviewed this task for it yet",
  judge_inconsistent: "the LLM judge changed its mind when the order was swapped",
};
type Choice = "left" | "right" | "tie" | "both_bad" | "skip";

/**
 * Blind pairwise review. The queue hands over the pair that most reduces
 * uncertainty; which variant produced which side is shown only after voting.
 */
export default function Review({ ctx }: { ctx: Ctx }) {
  const id = ctx.launchId;
  const [item, setItem] = useState<any>(null);
  const [progress, setProgress] = useState<any>(null);
  const [reveal, setReveal] = useState<any>(null);
  const [voted, setVoted] = useState<Choice | null>(null);
  const [reason, setReason] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);
  const [show, setShow] = useState<"output" | "diff">("output");
  const [auto, setAuto] = useState(() => localStorage.getItem("review.auto") === "1");
  // Past the starter set, reviewing is opt-in.
  const [more, setMore] = useState(false);
  useEffect(() => setMore(false), [id]);
  const reasonRef = useRef<HTMLInputElement>(null);

  const load = useCallback(async () => {
    if (!id) return;
    try {
      const r = await call("review_next", { launch_id: id });
      setItem(r.item); setProgress(r.progress); setReveal(null); setVoted(null); setReason(""); setError(null);
      setShow(r.item && !r.item.left.output.trim() && r.item.left.diff ? "diff" : "output");
    } catch (e: any) { setError(String(e.message ?? e)); }
    setLoaded(true);
  }, [id]);
  useEffect(() => { setLoaded(false); setItem(null); load(); }, [load]);

  const vote = useCallback(async (choice: Choice) => {
    if (!item || voted) return;
    try {
      const r = await call("review_vote", { launch_id: id, left_run: item.left.run_id, right_run: item.right.run_id, choice, reason });
      if (choice === "skip" || auto) { await load(); return; }
      setVoted(choice); setReveal(r);
    } catch (e: any) { setError(String(e.message ?? e)); }
  }, [item, voted, id, reason, auto, load]);

  const undo = async () => {
    if (!reveal) return;
    await call("review_undo", { judgment_id: reveal.judgment_id });
    setReveal(null); setVoted(null);
  };

  useEffect(() => {
    const on = (e: KeyboardEvent) => {
      const el = e.target as HTMLElement;
      if (el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.tagName === "SELECT" || e.metaKey || e.ctrlKey || e.altKey) return;
      const k = e.key.toLowerCase();
      if (voted) {
        if (k === "n" || k === "enter" || k === " ") { e.preventDefault(); load(); }
        if (k === "u") undo();
        return;
      }
      if (k === "a" || k === "1" || k === "arrowleft") vote("left");
      else if (k === "b" || k === "2" || k === "arrowright") vote("right");
      else if (k === "t" || k === "=") vote("tie");
      else if (k === "x") vote("both_bad");
      else if (k === "s") vote("skip");
      else if (k === "d") setShow((v) => (v === "output" ? "diff" : "output"));
      else if (k === "r") { e.preventDefault(); reasonRef.current?.focus(); }
    };
    window.addEventListener("keydown", on);
    return () => window.removeEventListener("keydown", on);
  });

  if (!id) return <div className="content"><Empty title="Nothing to review">Run an experiment with human review turned on.</Empty></div>;
  if (!loaded) return <div className="content"><ErrorNote error={error} /></div>;
  if (!item) {
    return (
      <div className="content">
        <ErrorNote error={error} />
        <Empty title={progress?.votes ? "Queue finished" : "Nothing to review in this launch"}>
          {progress?.votes
            ? `You cast ${progress.votes} vote${progress.votes === 1 ? "" : "s"}${progress.skipped ? ` and skipped ${progress.skipped}` : ""}. Every remaining pair is identical or already judged.`
            : "Pairs appear once two variants have finished runs of the same task."}
          <div style={{ marginTop: 14 }}><a className="btn" style={{ textDecoration: "none" }} href={`#/results/${id}`}>See the results</a></div>
        </Empty>
      </div>
    );
  }

  if (!more && !voted && progress && progress.recommended > 0 && progress.recommended_left === 0) {
    return (
      <div className="content">
        <Empty title={`Starter set done: ${progress.votes} pair${progress.votes === 1 ? "" : "s"} reviewed`}>
          <p style={{ maxWidth: "60ch", margin: "0 auto 14px" }}>
            Those were the pairs expected to settle the most. Your votes are already in the results, next to the judge's.
            You can stop here. {progress.useful_remaining > 0 ? `${progress.useful_remaining} more pairs sit in comparisons that are still undecided, if you want to add evidence.` : "Every comparison is already decided one way or the other."}
          </p>
          <div className="row" style={{ justifyContent: "center" }}>
            <a className="btn primary" style={{ textDecoration: "none" }} href={`#/results/${id}`}>See the results</a>
            <button className="btn" onClick={() => setMore(true)}>Review more pairs</button>
          </div>
        </Empty>
      </div>
    );
  }

  const setAnn = (side: "left" | "right") => (all: Annotation[]) => setItem((it: any) => ({ ...it, [side]: { ...it[side], annotations: all } }));
  const hasDiff = !!(item.left.diff || item.right.diff);
  const pane = (side: "left" | "right", letter: string) => {
    const s = item[side];
    const text = show === "diff" ? s.diff : s.output;
    return (
      <div className={`pane ${voted === side ? "won" : ""}`}>
        <header>
          <span className="side" aria-hidden>{letter}</span>
          {reveal ? <b>{side === "left" ? reveal.left_variant : reveal.right_variant}</b> : <span className="muted">Result {letter}</span>}
          {voted === side && <span className="chip good">your pick</span>}
          {reveal?.judge === side && <span className="chip accent">judge's pick</span>}
          <span className="grow" />
          <span className="faint small">{(s.output.trim().match(/\S+/g) ?? []).length} words</span>
          <button className="btn quiet sm" onClick={() => ctx.openRun(s.run_id, !reveal)}>How it got there</button>
        </header>
        <div className="scroll">
          {text ? <TextLines text={text} kind={show} annotations={s.annotations} runId={s.run_id} onChange={setAnn(side)} /> : <p className="muted" style={{ paddingLeft: 38 }}>{show === "diff" ? "No other files changed." : "No output."}</p>}
        </div>
      </div>
    );
  };

  return (
    <div className="content flush">
      <div className="review">
        <div className="task">
          <div className="row spread top">
            <div className="stack tight">
              <div className="row wrap">
                <h2>{item.task_id}</h2>
                <span className="muted small">
                  {progress.recommended_left > 0
                    ? `Pair ${Math.min(progress.votes + 1, progress.recommended)} of ${progress.recommended} in the starter set, chosen to settle the most. About a minute each.`
                    : `${progress.votes} reviewed. Past the starter set; stop whenever you like.`}
                </span>
              </div>
              <p>{item.task_prompt}</p>
              {item.expected_output && <p className="muted small">Expected: {item.expected_output}</p>}
            </div>
            <div className="row">
              {hasDiff && <Seg options={[["output", item.output_file ?? "Final message"], ["diff", "Other changes"]]} value={show} onChange={setShow} />}
            </div>
          </div>
          {item.why.length > 0 && <p className="faint small" style={{ marginTop: 4 }}>Picked because {item.why.map((w: string) => WHY[w] ?? w).join(", and ")}.</p>}
          <ErrorNote error={error} />
        </div>
        <div className="panes">{pane("left", "A")}{pane("right", "B")}</div>
        <div className="votebar">
          {!voted ? (
            <>
              <button className="btn primary" onClick={() => vote("left")}>Prefer A<kbd>A</kbd></button>
              <button className="btn primary" onClick={() => vote("right")}>Prefer B<kbd>B</kbd></button>
              <button className="btn" onClick={() => vote("tie")}>Tie<kbd>T</kbd></button>
              <button className="btn" onClick={() => vote("both_bad")}>Both bad<kbd>X</kbd></button>
              <button className="btn quiet" onClick={() => vote("skip")}>Skip<kbd>S</kbd></button>
              <input ref={reasonRef} type="text" placeholder="One-line reason (optional, press R)" value={reason} onChange={(e) => setReason(e.target.value)}
                onKeyDown={(e) => { if (e.key === "Escape" || e.key === "Enter") (e.target as HTMLInputElement).blur(); }} aria-label="Reason" />
              <label className="check small muted" title="Skip the reveal and go straight to the next pair">
                <input type="checkbox" checked={auto} onChange={(e) => { setAuto(e.target.checked); localStorage.setItem("review.auto", e.target.checked ? "1" : "0"); }} /> Next pair right after voting
              </label>
            </>
          ) : (
            <div className="reveal grow" style={{ flexWrap: "nowrap" }}>
              <span className="grow">A was <b>{reveal.left_variant}</b>, B was <b>{reveal.right_variant}</b>.{" "}
              {reveal.judge && <span className="muted">The judge {reveal.judge === "tie" ? "called it a tie" : `preferred ${reveal.judge === "left" ? "A" : "B"}`}{reveal.judge_consistent === false ? " (after disagreeing with itself across orders)" : ""}{reveal.judge_reason ? `: ${reveal.judge_reason}` : "."}</span>}
              </span>
              <button className="btn quiet" onClick={undo}>Undo vote<kbd>U</kbd></button>
              <button className="btn primary" onClick={load}>Next pair<kbd>N</kbd></button>
            </div>
          )}
        </div>
        <div className="votebar" style={{ borderTop: 0, paddingTop: 0 }}>
          <span className="faint small">Select text, or a line number, to leave a comment or reaction on it.{hasDiff ? " D switches between output and other changes." : ""}</span>
        </div>
      </div>
    </div>
  );
}
