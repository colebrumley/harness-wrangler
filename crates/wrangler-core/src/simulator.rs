//! The simulated user of a conversation task: a second model that plays
//! the person. It sees the persona, the hidden information, the dialog so
//! far and (optionally) the names of the tools the agent called, and
//! returns its reply, whether the task is done, whether it is satisfied,
//! and whether it gave away hidden information without being asked.
//!
//! The call goes through the same schema-validated `claude -p --json-schema`
//! path the judges use, so it stays on the user's subscription.

use crate::graders::judge::{backend_for, JudgeBackend};
use crate::model::{Conversation, JudgeCfg, Task};
use crate::runner::RunRecord;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

/// One side's contribution to the dialog, as the simulator sees it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DialogTurn {
    /// user | agent
    pub role: String,
    pub text: String,
    /// Names of the tools the agent called during this turn.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SimReply {
    #[serde(default)]
    pub reply: String,
    #[serde(default)]
    pub done: bool,
    #[serde(default)]
    pub satisfied: bool,
    #[serde(default)]
    pub revealed_hidden: bool,
}

pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "reply": {"type": "string"},
            "done": {"type": "boolean"},
            "satisfied": {"type": "boolean"},
            "revealed_hidden": {"type": "boolean"}
        },
        "required": ["reply", "done", "satisfied", "revealed_hidden"]
    })
}

pub fn prompt(task: &Task, conv: &Conversation, dialog: &[DialogTurn]) -> String {
    let mut s = String::from(
        "You are playing the USER in a conversation with an AI coding agent. Stay in character as the person described. You are not an assistant: do not help the agent, do not do its work, do not volunteer anything. You want what your opening request, the hidden information and the done_when condition say, and nothing else: do not invent new requirements, preferences or follow-up work.\n\n",
    );
    if !conv.persona.trim().is_empty() {
        s.push_str(&format!("<persona>\n{}\n</persona>\n\n", conv.persona.trim()));
    }
    s.push_str(&format!("You opened the conversation with this request:\n<opening_request>\n{}\n</opening_request>\n\n", task.prompt.trim()));
    if !conv.hidden.trim().is_empty() {
        s.push_str(&format!(
            "<hidden_information>\n{}\n</hidden_information>\nThe agent cannot see the hidden information. Reveal a piece of it ONLY when the agent asks a question that specifically calls for that piece. A vague question (\"anything else I should know?\") or no question at all does not call for it. If the agent does not ask, it does not find out.\n\n",
            conv.hidden.trim()
        ));
    }
    if !conv.done_when.trim().is_empty() {
        s.push_str(&format!("<done_when>\n{}\n</done_when>\n\n", conv.done_when.trim()));
    }
    s.push_str("<dialog>\n");
    for t in dialog {
        match t.role.as_str() {
            "user" => s.push_str(&format!("[user] {}\n", t.text.trim())),
            _ => {
                if !t.tools.is_empty() {
                    s.push_str(&format!("[agent used tools: {}]\n", t.tools.join(", ")));
                }
                s.push_str(&format!("[agent] {}\n", if t.text.trim().is_empty() { "(no message)" } else { t.text.trim() }));
            }
        }
    }
    s.push_str("</dialog>\n\n");
    s.push_str(
        "The agent has just finished its turn. Return:\n- `done`: true when the conversation should end here: the done_when condition holds, or the agent has finished and asked for nothing and you have nothing to add. When done is true, `reply` is the empty string.\n- `reply`: otherwise, what you say next, as this person would say it. Answer what was asked and nothing more. If the agent asked nothing and is not finished, a short nudge is enough.\n- `satisfied`: whether, as this person, you are satisfied with what the agent has done so far.\n- `revealed_hidden`: true only if your reply gives away hidden information that the agent did NOT specifically ask for.\n",
    );
    s
}

/// Why a dialog ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stop {
    /// The simulator said the conversation was over.
    Done,
    /// The simulator had nothing to say.
    NothingToAdd,
    /// The reply budget ran out before the simulator called it done.
    MaxUserTurns,
}

/// The state of one dialog: who said what, what it cost, and how it ended.
/// A runner calls `agent_turn_ended` at each turn boundary and sends back
/// whatever it returns.
pub struct Dialog<'a> {
    task: &'a Task,
    conv: &'a Conversation,
    cfg: JudgeCfg,
    backend: Box<dyn JudgeBackend>,
    cancel: Arc<AtomicBool>,
    pub turns: Vec<DialogTurn>,
    /// Replies sent to the agent, in order.
    pub replies: Vec<String>,
    pub satisfied: Option<bool>,
    pub hidden_revealed: u32,
    pub cost_usd: f64,
    pub stop: Option<Stop>,
}

impl<'a> Dialog<'a> {
    pub fn new(task: &'a Task, conv: &'a Conversation, model: &str, effort: Option<String>, cancel: &Arc<AtomicBool>) -> Dialog<'a> {
        let cfg = JudgeCfg { model: model.to_string(), effort: conv.effort.clone().or(effort), rubric: None, rubric_scoring: false, pairwise: false, pairwise_mode: None, pairwise_reps: None, transcript: None };
        Dialog {
            task,
            conv,
            backend: backend_for(&cfg),
            cfg,
            cancel: cancel.clone(),
            turns: vec![DialogTurn { role: "user".into(), text: task.prompt.clone(), tools: vec![] }],
            replies: vec![],
            satisfied: None,
            hidden_revealed: 0,
            cost_usd: 0.0,
            stop: None,
        }
    }

    /// The agent's turn is over: ask the simulated user what comes next.
    /// Returns the user's reply, or `None` when the dialog ends.
    pub fn agent_turn_ended(&mut self, text: &str, tools: Vec<String>) -> Result<Option<String>> {
        self.turns.push(DialogTurn { role: "agent".into(), text: text.to_string(), tools: if self.conv.sees_tools { tools } else { vec![] } });
        let reply = self.backend.complete(&prompt(self.task, self.conv, &self.turns), &schema(), &self.cfg, &self.cancel)?;
        self.cost_usd += reply.cost_usd;
        let r: SimReply = serde_json::from_value(reply.value)?;
        self.satisfied = Some(r.satisfied);
        if r.done {
            self.stop = Some(Stop::Done);
            return Ok(None);
        }
        if r.reply.trim().is_empty() {
            self.stop = Some(Stop::NothingToAdd);
            return Ok(None);
        }
        if self.replies.len() as u32 >= self.conv.max_user_turns {
            self.stop = Some(Stop::MaxUserTurns);
            return Ok(None);
        }
        if r.revealed_hidden {
            self.hidden_revealed += 1;
        }
        self.turns.push(DialogTurn { role: "user".into(), text: r.reply.clone(), tools: vec![] });
        self.replies.push(r.reply.clone());
        Ok(Some(r.reply))
    }

    /// Record the dialog's outcome on the run: metrics, and the limit when
    /// the reply budget ran out (graded as it stands, like a timeout).
    pub fn finish(&self, rec: &mut RunRecord) {
        let m = &mut rec.metrics;
        m.insert("user_turns".into(), rec.transcript.user_turns() as f64);
        m.insert("questions_asked".into(), rec.transcript.questions_asked() as f64);
        if let Some(s) = self.satisfied {
            m.insert("user_satisfied".into(), if s { 1.0 } else { 0.0 });
        }
        m.insert("hidden_revealed".into(), self.hidden_revealed as f64);
        m.insert("simulator_cost_usd".into(), self.cost_usd);
        if self.stop == Some(Stop::MaxUserTurns) {
            rec.limit_hit.get_or_insert(format!("stopped at max_user_turns ({}) before the simulated user called the task done", self.conv.max_user_turns));
        }
    }

    /// What is stored with the run so a result can be traced to its simulator.
    pub fn record(&self) -> Value {
        json!({"model": self.cfg.model, "effort": self.cfg.effort, "persona": self.conv.persona, "hidden": self.conv.hidden, "done_when": self.conv.done_when, "max_user_turns": self.conv.max_user_turns,
            "sees_tools": self.conv.sees_tools, "user_turns": self.replies.len(), "stop": self.stop.as_ref().map(|s| match s { Stop::Done => "done", Stop::NothingToAdd => "nothing_to_add", Stop::MaxUserTurns => "max_user_turns" }),
            "satisfied": self.satisfied, "hidden_revealed": self.hidden_revealed, "cost_usd": self.cost_usd})
    }
}

/// Scripted replies for the mock simulator (`model = "mock"`): each
/// `mock-reply:` line of the hidden text is sent in turn, then the dialog is
/// done. A reply starting with `!` is flagged as revealing hidden information.
pub fn mock_reply(prompt: &str) -> Value {
    let section = |tag: &str| -> &str {
        let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
        match (prompt.find(&open), prompt.rfind(&close)) {
            (Some(a), Some(b)) if b > a => &prompt[a + open.len()..b],
            _ => "",
        }
    };
    let script: Vec<&str> = section("hidden_information").lines().filter_map(|l| l.trim().strip_prefix("mock-reply:")).map(|s| s.trim()).collect();
    // The dialog opens with the request; every later [user] line is a reply already sent.
    let sent = section("dialog").lines().filter(|l| l.starts_with("[user] ")).count().saturating_sub(1);
    match script.get(sent) {
        Some(line) => json!({"reply": line.trim_start_matches('!').trim(), "done": false, "satisfied": false, "revealed_hidden": line.starts_with('!')}),
        None => json!({"reply": "", "done": true, "satisfied": true, "revealed_hidden": false}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(hidden: &str, max: u32) -> Task {
        Task::parse(&format!("id = 't'\nprompt = 'Something is broken in slugify, fix it.'\n[conversation]\npersona = 'A busy maintainer.'\nhidden = '''\n{hidden}'''\nmax_user_turns = {max}\nmodel = 'mock'\ndone_when = 'The tests pass.'\n")).unwrap()
    }

    #[test]
    fn prompt_carries_persona_hidden_and_tools() {
        let t = task("The failing test is the unicode case.", 6);
        let conv = t.conversation.as_ref().unwrap();
        let dialog = vec![DialogTurn { role: "user".into(), text: t.prompt.clone(), tools: vec![] }, DialogTurn { role: "agent".into(), text: "Which test fails?".into(), tools: vec!["Bash".into(), "Read".into()] }];
        let p = prompt(&t, conv, &dialog);
        for needle in ["<persona>\nA busy maintainer.", "<hidden_information>\nThe failing test is the unicode case.", "ONLY when the agent asks", "<done_when>\nThe tests pass.", "[agent used tools: Bash, Read]\n[agent] Which test fails?", "[user] Something is broken"] {
            assert!(p.contains(needle), "missing {needle:?} in\n{p}");
        }
    }

    #[test]
    fn scripted_dialog_runs_to_done_and_respects_the_budget() {
        let cancel = Arc::new(AtomicBool::new(false));
        let t = task("mock-reply: The unicode one.\nmock-reply: !Also the hyphen thing, since you did not ask.\n", 6);
        let mut d = Dialog::new(&t, t.conversation.as_ref().unwrap(), "mock", None, &cancel);
        assert_eq!(d.agent_turn_ended("Which test fails?", vec!["Bash".into()]).unwrap().as_deref(), Some("The unicode one."));
        assert_eq!(d.agent_turn_ended("Fixed that.", vec![]).unwrap().as_deref(), Some("Also the hyphen thing, since you did not ask."));
        assert_eq!(d.agent_turn_ended("Done.", vec![]).unwrap(), None);
        assert_eq!((d.stop.clone(), d.satisfied, d.hidden_revealed, d.replies.len()), (Some(Stop::Done), Some(true), 1, 2));
        assert_eq!(d.record()["stop"], "done");

        // One reply allowed, two scripted: the run stops at the budget, unsatisfied.
        let t = task("mock-reply: one\nmock-reply: two\n", 1);
        let mut d = Dialog::new(&t, t.conversation.as_ref().unwrap(), "mock", None, &cancel);
        assert!(d.agent_turn_ended("a?", vec![]).unwrap().is_some());
        assert_eq!(d.agent_turn_ended("b?", vec![]).unwrap(), None);
        assert_eq!((d.stop.clone(), d.satisfied), (Some(Stop::MaxUserTurns), Some(false)));
        let mut rec = RunRecord::default();
        d.finish(&mut rec);
        assert!(rec.limit_hit.as_deref().unwrap().contains("max_user_turns (1)"));
        assert_eq!(rec.metrics["user_satisfied"], 0.0);
    }
}
