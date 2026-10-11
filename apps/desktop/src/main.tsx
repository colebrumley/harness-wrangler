import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import "./styles.css";
import { inTauri, shell } from "./api";

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);

// Scripted runs of the desktop app (see `ui_script` in the shell): press the
// named buttons in order, exactly as a click would, and report each screen.
if (inTauri) {
  const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
  shell<string[]>("ui_script").then(async (steps) => {
    if (!steps.length) return;
    const report: { step: string; pressed: boolean; heading: string; text: string }[] = [];
    await sleep(2500);
    for (const step of steps) {
      let button: HTMLButtonElement | undefined;
      for (let i = 0; i < 40 && !button; i++) {
        button = Array.from(document.querySelectorAll("button")).find((b) => b.textContent?.trim() === step && !b.disabled);
        if (!button) await sleep(250);
      }
      button?.click();
      await sleep(2500);
      report.push({ step, pressed: !!button, heading: document.querySelector("h1")?.textContent ?? "", text: document.body.innerText.slice(0, 400) });
    }
    await shell("ui_script_report", { report });
  }).catch(() => {});
}
