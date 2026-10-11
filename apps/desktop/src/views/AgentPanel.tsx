import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import React, { useEffect, useRef, useState } from "react";
import { shell } from "../api";

function bytes(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

const STARTERS: [string, string][] = [
  ["Draft tasks", "Look at this repo and propose three to five tasks for testing the skill or instructions I care about. Ask me what I want to test if it is not obvious, then write the task files and fixtures."],
  ["Explain my latest results", "Read the report and advice for my latest launch with the wrangler CLI and explain in plain terms what was found and how sure we can be."],
  ["What should I do next?", "Check where this Harness Wrangler project stands (tasks, experiments, launches) and tell me the next step."],
];

/**
 * A terminal running Claude Code in the project folder. The agent is told
 * how Harness Wrangler's files and commands work, so you can ask it to
 * draft tasks and experiments or explain results without leaving the app.
 */
export default function AgentPanel({ projectKey, onClose, onResizeStart, onNudge }: { projectKey: number; onClose: () => void; onResizeStart: (e: React.PointerEvent) => void; onNudge: (by: number) => void }) {
  const host = useRef<HTMLDivElement>(null);
  const [error, setError] = useState<string | null>(null);
  const [exited, setExited] = useState(false);
  const [session, setSession] = useState(0);

  useEffect(() => {
    if (!host.current) return;
    const term = new Terminal({ fontFamily: 'ui-monospace, "SF Mono", Menlo, monospace', fontSize: 12.5, cursorBlink: true, scrollback: 5000, theme: { background: "#10161c", foreground: "#dfe6ec", cursor: "#7fc0dd" } });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(host.current);
    fit.fit();
    let off: (() => void)[] = [];
    let gone = false;
    setExited(false);
    setError(null);
    (async () => {
      const { listen } = await import("@tauri-apps/api/event");
      const a = await listen<string>("agent-output", (e) => term.write(bytes(e.payload)));
      const b = await listen("agent-exit", () => setExited(true));
      if (gone) { a(); b(); return; }
      off = [a, b];
      try {
        const backlog = await shell<string>("agent_start", { cols: term.cols, rows: term.rows });
        if (backlog) term.write(bytes(backlog));
        term.focus();
      } catch (e: any) { setError(String(e.message ?? e)); }
    })();
    const data = term.onData((d) => { shell("agent_input", { data: d }).catch(() => {}); });
    const resize = new ResizeObserver(() => { try { fit.fit(); shell("agent_resize", { cols: term.cols, rows: term.rows }).catch(() => {}); } catch { /* panel hidden */ } });
    resize.observe(host.current);
    return () => { gone = true; off.forEach((f) => f()); data.dispose(); resize.disconnect(); term.dispose(); };
  }, [projectKey, session]);

  // Enter goes separately, after the text: sent together, the terminal UI
  // takes the whole thing as a paste and leaves it unsent in the prompt.
  const say = async (text: string) => {
    try {
      await shell("agent_input", { data: text });
      await new Promise((r) => setTimeout(r, 250));
      await shell("agent_input", { data: "\r" });
    } catch (e: any) { setError(String(e.message ?? e)); }
  };
  return (
    <aside className="agent" aria-label="Agent terminal">
      <div className="resizer" role="separator" aria-orientation="vertical" aria-label="Resize the agent panel" tabIndex={0} title="Drag to resize"
        onPointerDown={onResizeStart}
        onKeyDown={(e) => { if (e.key === "ArrowLeft") onNudge(40); if (e.key === "ArrowRight") onNudge(-40); }} />
      <header>
        <span className="grow">Agent in this project</span>
        {exited && <button className="btn sm" onClick={() => setSession((s) => s + 1)}>Start again</button>}
        <button className="btn sm" onClick={async () => { await shell("agent_stop"); setSession((s) => s + 1); }}>Restart</button>
        <button className="btn sm" onClick={onClose}>Hide</button>
      </header>
      {error && <div className="hint" role="alert">{error}</div>}
      <div className="term" ref={host} />
      <div className="hint">
        Claude Code, briefed on this tool's file formats and commands. Ask it anything, or start with:{" "}
        {STARTERS.map(([label, text]) => <button key={label} onClick={() => say(text)}>{label}</button>)}
      </div>
    </aside>
  );
}
